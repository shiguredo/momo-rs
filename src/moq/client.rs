//! MOQT の Session と QUIC I/O を繋ぐクライアント
//!
//! Sans-I/O な `shiguredo_moqt::Session` を核に、制御ストリームと bidi request stream を
//! `s2n-quic` 上で駆動する。publisher / subscriber の双方が共有する。
//!
//! ストリームの対応は draft-ietf-moq-transport-21 §6.3 (Session initialization) /
//! §6.4 (Streams) に従う。draft 由来のため将来の改版で変わる可能性がある。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};

use bytes::Bytes;
use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::message::common::{Location, TrackNamespace};
use shiguredo_moqt::message::{ControlMessage, ReasonPhrase};
use shiguredo_moqt::message_parameter::{
    MessageParameter, MessageParameterValue, MessageParameters, PARAM_SUBSCRIBER_PRIORITY,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{
    DataStreamId, DataStreamResetReason, FetchState, RecvDataStreamError, RequestStreamEnd,
    SendRequestError, SessionError as MoqtSessionError, SessionEvent, SessionState,
    SubscriptionState, TrackDataAcceptance, Transport,
};
use shiguredo_moqt::stream::decoder::DecodedSubgroupObject;
use shiguredo_moqt::stream::encode_control_stream_setup;
use shiguredo_moqt::stream::fetch::{
    FetchHeader, FetchPriorContext, FetchStreamEntry, FetchStreamObject, FetchSubgroupIdMode,
};
use shiguredo_moqt::stream::subgroup::SubgroupHeader;
use shiguredo_moqt::track_properties::TrackProperties;
use tokio::sync::{mpsc, oneshot};

use crate::error::BoxError;
use crate::moq::error::MoqError;
use crate::moq::transport::{
    BidiStreamAcceptor, RecvChunk, RecvStream, SendStream, StreamAcceptor, StreamHandle,
};
use crate::moq::url::ServerUrl;

/// Subscriber Priority の既定値
///
/// draft-ietf-moq-transport-21 §9.20.11 (SUBSCRIBER_PRIORITY) の中央値を使う。
const DEFAULT_SUBSCRIBER_PRIORITY: u8 = 128;

/// SETUP の MOQT_IMPLEMENTATION に載せる実装名
const IMPLEMENTATION_NAME: &str = "momo-rs";

/// オブジェクト送信前のフィルタ評価の結果
///
/// draft-ietf-moq-transport-21 §3.3.3 (Combining Filters): publisher はすべてのフィルタを
/// 通過したオブジェクトだけを転送しなければならない。draft 由来のため将来の改版で
/// 変わる可能性がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "Skip の場合はワイヤ送信を省略する必要がある"]
pub enum ObjectFilterOutcome {
    /// フィルタを通過したため、ワイヤへ送信してよい
    Pass,
    /// フィルタ不通過のため、ワイヤ送信をスキップする
    Skip,
}

/// bidi ストリームからメインループへの通知
///
/// 自側が開始した request stream 上のメッセージ (`Response`) と、peer が開始した
/// request stream の先頭メッセージ (`PeerRequest`) を同じチャネルで運ぶ。同一チャネルに
/// 載せることで、ある request stream の先頭メッセージと後続メッセージの処理順が
/// 入れ替わらないことを保証する。
enum BidiMessage {
    /// 自側が開始した request stream 上のメッセージ
    Response(u64, Result<StreamRead<ControlMessage>, MoqError>),
    /// peer が開始した request stream の先頭メッセージ
    PeerRequest {
        /// 開始メッセージに含まれる Request ID
        request_id: u64,
        /// 開始メッセージ (SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS)
        message: ControlMessage,
        /// 応答を書き戻すための送信半
        send: SendStream,
        /// 受信方向の STOP_SENDING を依頼するチャネル
        stop_tx: mpsc::Sender<StopSendingCommand>,
    },
}

/// bidi 受信タスクへの STOP_SENDING 指示
struct StopSendingCommand {
    /// Stream Reset Error Code (draft-ietf-moq-transport-21 §12.5)
    error_code: u64,
    /// 送出結果の完了通知
    ack: oneshot::Sender<Result<(), MoqError>>,
}

/// 制御ストリーム受信タスクからメインループへの通知
type ControlIncoming = Result<StreamRead<ControlMessage>, MoqError>;

/// peer から届いた要求
#[derive(Debug)]
pub struct IncomingRequest {
    /// 要求の Request ID
    pub request_id: u64,
    /// 要求メッセージ
    pub message: ControlMessage,
}

/// peer から届いた REQUEST_UPDATE
#[derive(Debug)]
pub struct IncomingRequestUpdate {
    /// 対象 request の Request ID
    pub request_id: u64,
}

/// [`MoqtClient::next_event`] が返すイベント
#[derive(Debug)]
pub enum ClientEvent {
    /// session の注目イベント (GOAWAY / close / PUBLISH_DONE)
    Session(SessionEvent),
    /// peer から届いた要求 (relay が転送した SUBSCRIBE / FETCH)
    Request(IncomingRequest),
    /// peer から届いた REQUEST_UPDATE
    RequestUpdate(IncomingRequestUpdate),
}

/// SUBSCRIBE の結果
pub struct SubscribeResult {
    /// 自側の Request ID
    pub request_id: u64,
    /// SUBSCRIBE_OK で確定した Track Alias
    pub track_alias: u64,
}

/// FETCH の結果
pub struct FetchResult {
    /// 自側の Request ID
    pub request_id: u64,
}

/// ストリームから読み出した値または終端
pub enum StreamRead<T> {
    /// 読み出した値
    Value(T),
    /// ストリームの終端
    Closed(RequestStreamEnd),
}

/// 制御メッセージをインクリメンタルに読み出すリーダー
///
/// QUIC のフレーム境界と MOQT のメッセージ境界は一致しないため、
/// 受信データを蓄積しながらデコードする。
pub struct ControlStream {
    stream: RecvStream,
    decoder: MessageDecoder,
}

impl ControlStream {
    /// 受信ストリームからリーダーを作る
    pub fn new(stream: RecvStream) -> Self {
        Self {
            stream,
            decoder: MessageDecoder::new(),
        }
    }

    /// ストリーム先頭のストリームタイプ varint を読む
    pub async fn read_stream_type(&mut self) -> Result<StreamRead<u64>, MoqError> {
        loop {
            if let Some(value) = self
                .decoder
                .try_decode_varint()
                .map_err(|e| MoqError::Session(format!("failed to decode stream type: {e}")))?
            {
                return Ok(StreamRead::Value(value));
            }
            match self.stream.receive_chunk().await? {
                RecvChunk::Data(data) => self.decoder.push(&data),
                RecvChunk::End(end) => return Ok(StreamRead::Closed(end)),
            }
        }
    }

    /// 制御メッセージを 1 つ読む
    pub async fn recv_message(&mut self) -> Result<StreamRead<ControlMessage>, MoqError> {
        loop {
            if let Some(message) = self
                .decoder
                .try_decode_message()
                .map_err(|e| MoqError::Session(format!("failed to decode message: {e}")))?
            {
                return Ok(StreamRead::Value(message));
            }
            match self.stream.receive_chunk().await? {
                RecvChunk::Data(data) => self.decoder.push(&data),
                RecvChunk::End(end) => return Ok(StreamRead::Closed(end)),
            }
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    pub fn stop_sending(&mut self, error_code: u64) -> Result<(), MoqError> {
        self.stream.stop_sending(error_code)
    }
}

/// data stream から Session の data plane を駆動するためのハンドル
#[derive(Clone)]
pub struct DataPlaneHandle {
    session: Arc<StdMutex<Session>>,
}

impl DataPlaneHandle {
    fn new(session: Arc<StdMutex<Session>>) -> Self {
        Self { session }
    }

    /// subgroup stream の open を Session に通知する
    pub fn send_subgroup_header(
        &self,
        stream_id: DataStreamId,
        request_id: u64,
        header: &SubgroupHeader,
    ) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .send_subgroup_header(stream_id, request_id, header)
            .map_err(|e| MoqError::Session(format!("send_subgroup_header: {e}")).into())
    }

    /// subgroup object を Session に通知し、フィルタ評価の結果を返す
    pub fn send_subgroup_object(
        &self,
        stream_id: DataStreamId,
        object_id: u64,
        properties_bytes: Option<&[u8]>,
    ) -> Result<ObjectFilterOutcome, BoxError> {
        let mut session = lock_session(&self.session);
        match session.send_subgroup_object(stream_id, object_id, properties_bytes) {
            Ok(()) => Ok(ObjectFilterOutcome::Pass),
            Err(SendRequestError::LocalFilterMismatch) => Ok(ObjectFilterOutcome::Skip),
            Err(e) => Err(MoqError::Session(format!("send_subgroup_object: {e}")).into()),
        }
    }

    /// uni data stream の close を Session に通知する (publisher 側)
    pub fn send_data_stream_closed(
        &self,
        stream_id: DataStreamId,
        end: RequestStreamEnd,
    ) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .send_data_stream_closed(stream_id, end)
            .map_err(|e| MoqError::Session(format!("send_data_stream_closed: {e}")).into())
    }

    /// FETCH 応答用 uni stream の open を Session に通知する (publisher 側)
    pub fn send_fetch_header(
        &self,
        stream_id: DataStreamId,
        request_id: u64,
    ) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .send_fetch_header(stream_id, request_id)
            .map_err(|e| MoqError::Session(format!("send_fetch_header: {e}")).into())
    }

    /// FETCH 応答 stream 上の Object 送信を Session に通知する (publisher 側)
    pub fn send_fetch_object(&self, stream_id: DataStreamId) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .send_fetch_object(stream_id)
            .map_err(|e| MoqError::Session(format!("send_fetch_object: {e}")).into())
    }

    /// FETCH 応答 stream の終端を Session に通知する (publisher 側)
    pub fn send_fetch_data_stream_closed(&self, stream_id: DataStreamId) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .send_fetch_data_stream_closed(stream_id)
            .map_err(|e| MoqError::Session(format!("send_fetch_data_stream_closed: {e}")).into())
    }

    /// uni data stream の type を Session に通知する (subscriber 側)
    pub fn recv_data_stream_type(
        &self,
        stream_id: DataStreamId,
        stream_type: u64,
    ) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        match session.recv_data_stream_type(stream_id, stream_type) {
            Ok(_) => Ok(()),
            Err(RecvDataStreamError::BeforeSessionEstablished) => Err(MoqError::Session(
                "data stream arrived before session established".to_owned(),
            )
            .into()),
            Err(RecvDataStreamError::InvalidInput(e) | RecvDataStreamError::Session(e)) => {
                Err(MoqError::Session(format!("recv_data_stream_type: {e}")).into())
            }
        }
    }

    /// subgroup header を Session に通知する (subscriber 側)
    pub fn recv_subgroup_header(
        &self,
        stream_id: DataStreamId,
        header: &SubgroupHeader,
    ) -> Result<TrackDataAcceptance, BoxError> {
        let mut session = lock_session(&self.session);
        session
            .recv_subgroup_header(stream_id, header)
            .map_err(|e| MoqError::Session(format!("recv_subgroup_header: {e}")).into())
    }

    /// subgroup object を Session に通知する (subscriber 側)
    ///
    /// 戻り値は帰属判定。`FilteredOut` / `Discarded` の object も wire 上は payload を
    /// 持つため、呼び出し側は payload を読み出して消費する。
    pub fn recv_subgroup_object(
        &self,
        stream_id: DataStreamId,
        object: &DecodedSubgroupObject,
    ) -> Result<TrackDataAcceptance, BoxError> {
        let mut session = lock_session(&self.session);
        session
            .recv_subgroup_object(stream_id, object)
            .map_err(|e| MoqError::Session(format!("recv_subgroup_object: {e}")).into())
    }

    /// fetch header を Session に通知する (subscriber 側)
    pub fn recv_fetch_header(
        &self,
        stream_id: DataStreamId,
        header: &FetchHeader,
    ) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .recv_fetch_header(stream_id, header)
            .map_err(|e| MoqError::Session(format!("recv_fetch_header: {e}")).into())
    }

    /// fetch entry を Session に通知する (subscriber 側)
    pub fn recv_fetch_entry(&self, stream_id: DataStreamId) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .recv_fetch_entry(stream_id)
            .map_err(|e| MoqError::Session(format!("recv_fetch_entry: {e}")).into())
    }

    /// uni data stream の close を Session に通知する (subscriber 側)
    pub fn recv_data_stream_closed(
        &self,
        stream_id: DataStreamId,
        end: RequestStreamEnd,
    ) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .recv_data_stream_closed(stream_id, end)
            .map_err(|e| MoqError::Session(format!("recv_data_stream_closed: {e}")).into())
    }

    /// data stream へ STOP_SENDING を送信する (subscriber 側)
    pub fn send_data_stream_stop_sending(&self, stream_id: DataStreamId) -> Result<(), BoxError> {
        let mut session = lock_session(&self.session);
        session
            .send_data_stream_stop_sending(stream_id)
            .map_err(|e| MoqError::Session(format!("send_data_stream_stop_sending: {e}")).into())
    }
}

/// publisher / subscriber 共通の MoQT クライアント
pub struct MoqtClient {
    session: Arc<StdMutex<Session>>,
    control_send: SendStream,
    handle: StreamHandle,
    /// request_id → bidi 送信半
    bidi_sends: HashMap<u64, SendStream>,
    /// request_id → STOP_SENDING 依頼チャネル
    bidi_stop_txs: HashMap<u64, mpsc::Sender<StopSendingCommand>>,
    /// 回収待ちの request stream
    closed_request_streams: HashSet<u64>,
    /// peer から届いた未処理の要求
    incoming_requests: VecDeque<IncomingRequest>,
    /// peer から届いた未処理の REQUEST_UPDATE
    incoming_updates: VecDeque<IncomingRequestUpdate>,
    /// アプリが観測する notable イベント
    notable_events: VecDeque<SessionEvent>,
    control_rx: mpsc::Receiver<ControlIncoming>,
    bidi_tx: mpsc::Sender<BidiMessage>,
    bidi_rx: mpsc::Receiver<BidiMessage>,
    /// 自側から close を開始したかどうか
    ///
    /// 自側の `close` で発生する `CloseSession` はエラーではないため区別する。
    closing: bool,
}

/// アプリ (`next_event`) が観測する notable イベントか
fn is_notable_event(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::GoawayReceived { .. }
            | SessionEvent::CloseSession(_)
            | SessionEvent::PublishDoneReceived { .. }
    )
}

/// `Session` を排他ロックして取り出す
///
/// `Session` は sans-I/O な状態機械で、control / bidi / data plane の複数の非同期タスクから
/// 共有される。各操作は await をまたがず短時間で完結するため、チャネルで単一所有者へ
/// 操作を依頼する構成より Mutex の方が状態の一貫性を保ったまま構成を単純にできる。
fn lock_session(session: &Arc<StdMutex<Session>>) -> std::sync::MutexGuard<'_, Session> {
    session.lock().expect("session mutex must not be poisoned")
}

/// request の開始メッセージから Request ID を取り出す
///
/// bidi request stream の先頭メッセージとして許されるのは SUBSCRIBE / PUBLISH /
/// FETCH / TRACK_STATUS の 4 種類である
/// (draft-ietf-moq-transport-21 §6.3 (Session initialization))。
fn request_id_of(message: &ControlMessage) -> Option<u64> {
    match message {
        ControlMessage::Subscribe(m) => Some(m.request_id),
        ControlMessage::Publish(m) => Some(m.request_id),
        ControlMessage::Fetch(m) => Some(m.request_id),
        ControlMessage::TrackStatus(m) => Some(m.request_id),
        _ => None,
    }
}

/// SETUP に載せる Setup Options を構築する
///
/// draft-ietf-moq-transport-21 §9.1.1 (AUTHORITY) / §9.1.2 (PATH) は QUIC 直接接続では
/// authority と path を含めると定める。データグラムを送らないため MAX_FILTER_RANGES 等は
/// 載せない。
fn build_setup_options(server: &ServerUrl) -> shiguredo_moqt::parameter::SetupOptions {
    use shiguredo_moqt::parameter::{
        SETUP_OPTION_AUTHORITY, SETUP_OPTION_MOQT_IMPLEMENTATION, SETUP_OPTION_PATH, SetupOption,
        SetupOptionValue, SetupOptions,
    };

    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: SETUP_OPTION_AUTHORITY,
        value: SetupOptionValue::Bytes(server.authority().as_bytes().to_vec()),
    });
    if let Some(path) = server.path_option() {
        options.push(SetupOption {
            option_type: SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(path.as_bytes().to_vec()),
        });
    }
    options.push(SetupOption {
        option_type: SETUP_OPTION_MOQT_IMPLEMENTATION,
        value: SetupOptionValue::Bytes(IMPLEMENTATION_NAME.as_bytes().to_vec()),
    });
    options
}

impl MoqtClient {
    /// QUIC 接続上で MOQT session を確立する
    ///
    /// SETUP の交換までを完了させ、制御ストリームと peer 起動 request stream の
    /// 受信タスクを起動する。戻り値の [`StreamAcceptor`] は data stream を受信する側
    /// (subscriber) が使う。
    pub async fn establish(
        connection: s2n_quic::connection::Connection,
        server: &ServerUrl,
    ) -> Result<(Self, StreamAcceptor), BoxError> {
        let (handle, acceptor) = connection.split();
        let (bidi_acceptor, mut recv_acceptor) = acceptor.split();
        let handle = StreamHandle::new(handle);

        // 自側の制御ストリームを開く
        let mut control_send = handle.open_send_stream().await?;

        let mut session = Session::new_client(Transport::Quic, build_setup_options(server))
            .map_err(|e| MoqError::Session(format!("failed to create session: {e}")))?;

        // SETUP は Session が生成したメッセージをそのまま送出する。
        // 独自に組み立てると Session の内部状態と wire がずれるため、必ずイベントから取り出す
        let setup = match session.poll_event() {
            Some(SessionEvent::SendControl(message)) => message,
            other => {
                return Err(MoqError::Session(format!(
                    "expected SendControl as initial event, got {other:?}"
                ))
                .into());
            }
        };
        control_send
            .send(Bytes::from(encode_control_stream_setup(&setup).map_err(
                |e| MoqError::Session(format!("failed to encode SETUP: {e}")),
            )?))
            .await?;
        tracing::info!(target: "moq", "sent SETUP message");

        // 相手の制御ストリームを待つ
        let recv_stream = recv_acceptor
            .accept_receive_stream()
            .await
            .map_err(|e| MoqError::Quic(format!("failed to accept control stream: {e}")))?
            .ok_or_else(|| MoqError::Quic("connection closed before control stream".into()))?;
        let mut control_recv = ControlStream::new(RecvStream::new(recv_stream));
        let stream_type = match control_recv.read_stream_type().await? {
            StreamRead::Value(stream_type) => stream_type,
            StreamRead::Closed(_) => {
                return Err(MoqError::Session(
                    "control stream closed before stream type".to_owned(),
                )
                .into());
            }
        };
        session
            .recv_control_stream_type(stream_type)
            .map_err(|e| MoqError::Session(format!("unexpected control stream type: {e}")))?;

        // 相手の SETUP を待つ
        let peer_setup = match control_recv.recv_message().await? {
            StreamRead::Value(message) => message,
            StreamRead::Closed(_) => {
                return Err(
                    MoqError::Session("control stream closed before SETUP".to_owned()).into(),
                );
            }
        };
        session
            .recv_control(peer_setup)
            .map_err(|e| MoqError::Session(format!("failed to handle peer SETUP: {e}")))?;
        if session.state() != SessionState::Established {
            return Err(MoqError::Session(format!(
                "session is not established after SETUP exchange: {:?}",
                session.state()
            ))
            .into());
        }
        // Established イベントはここで回収しておく
        while let Some(event) = session.poll_event() {
            if matches!(event, SessionEvent::Established) {
                break;
            }
        }
        tracing::info!(target: "moq", state = ?session.state(), "MOQT session established");

        let session = Arc::new(StdMutex::new(session));

        // 制御ストリームの受信をタスク化する
        let (control_rx, _control_task) = spawn_control_recv_task(control_recv);
        let (bidi_tx, bidi_rx) = mpsc::channel::<BidiMessage>(64);
        let _peer_bidi_task =
            spawn_peer_bidi_accept_task(BidiStreamAcceptor::new(bidi_acceptor), bidi_tx.clone());

        let client = Self {
            session,
            control_send,
            handle,
            bidi_sends: HashMap::new(),
            bidi_stop_txs: HashMap::new(),
            closed_request_streams: HashSet::new(),
            incoming_requests: VecDeque::new(),
            incoming_updates: VecDeque::new(),
            notable_events: VecDeque::new(),
            control_rx,
            bidi_tx,
            bidi_rx,
            closing: false,
        };
        Ok((client, StreamAcceptor::new(recv_acceptor)))
    }

    /// ストリーム開設用ハンドルのクローン
    pub fn handle(&self) -> StreamHandle {
        self.handle.clone()
    }

    /// data plane 通知用ハンドルのクローン
    pub fn data_plane(&self) -> DataPlaneHandle {
        DataPlaneHandle::new(Arc::clone(&self.session))
    }

    /// PUBLISH を発行し、応答が返るまで待つ (publisher 側)
    ///
    /// draft-ietf-moq-transport-21 §9.1 (PUBLISH) / §9.3 (REQUEST_OK)。
    pub async fn publish_track(
        &mut self,
        namespace: TrackNamespace,
        track_name: Vec<u8>,
        track_alias: u64,
    ) -> Result<u64, BoxError> {
        let request_id = {
            let mut session = lock_session(&self.session);
            session
                .send_publish(
                    namespace,
                    track_name,
                    track_alias,
                    MessageParameters::new(),
                    TrackProperties::new(),
                )
                .map_err(|e| MoqError::Session(format!("send_publish: {e}")))?
        };
        self.drain_events().await?;
        tracing::info!(
            target: "moq",
            request_id = request_id,
            track_alias = track_alias,
            "sent PUBLISH"
        );
        self.wait_subscription_resolved(request_id).await?;
        Ok(request_id)
    }

    /// PUBLISH_DONE を送信する (publisher 側)
    ///
    /// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE)。
    pub async fn send_publish_done(
        &mut self,
        request_id: u64,
        status_code: u64,
        reason: &str,
    ) -> Result<(), BoxError> {
        let reason = ReasonPhrase::new(reason)
            .map_err(|e| MoqError::Session(format!("invalid reason phrase: {e}")))?;
        let stream_count = {
            let session = lock_session(&self.session);
            session
                .subscription(request_id)
                .map(|sub| sub.stream_counts.published_count)
                .ok_or_else(|| MoqError::Session(format!("subscription not found: {request_id}")))?
        };
        {
            let mut session = lock_session(&self.session);
            session
                .send_publish_done(request_id, status_code, stream_count, reason)
                .map_err(|e| MoqError::Session(format!("send_publish_done: {e}")))?;
        }
        self.drain_events().await?;
        tracing::info!(target: "moq", request_id = request_id, "sent PUBLISH_DONE");
        Ok(())
    }

    /// トラックを SUBSCRIBE する (subscriber 側)
    ///
    /// draft-ietf-moq-transport-21 §9.4 (SUBSCRIBE)。
    pub async fn subscribe_track(
        &mut self,
        namespace: TrackNamespace,
        track_name: Vec<u8>,
    ) -> Result<SubscribeResult, BoxError> {
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_SUBSCRIBER_PRIORITY,
            value: MessageParameterValue::Uint8(DEFAULT_SUBSCRIBER_PRIORITY),
        });
        let request_id = {
            let mut session = lock_session(&self.session);
            session
                .send_subscribe(namespace, track_name, parameters)
                .map_err(|e| MoqError::Session(format!("send_subscribe: {e}")))?
        };
        self.drain_events().await?;
        self.wait_subscription_resolved(request_id).await?;
        let (request_id, track_alias) = {
            let session = lock_session(&self.session);
            let subscription = session.subscription(request_id).ok_or_else(|| {
                MoqError::Session(format!("subscription disappeared: {request_id}"))
            })?;
            let track_alias = subscription
                .track_alias
                .ok_or_else(|| MoqError::Session("SUBSCRIBE_OK without track alias".to_owned()))?;
            (subscription.request_id, track_alias)
        };
        tracing::info!(
            target: "moq",
            request_id = request_id,
            track_alias = track_alias,
            "SUBSCRIBE_OK received"
        );
        Ok(SubscribeResult {
            request_id,
            track_alias,
        })
    }

    /// FETCH を発行し、応答が返るまで待つ (subscriber 側)
    ///
    /// 範囲は LOCATION_FILTER パラメータで指定する
    /// (draft-ietf-moq-transport-21 §9.11 (FETCH) / §9.20.6 (LOCATION_FILTER))。
    pub async fn fetch(
        &mut self,
        namespace: TrackNamespace,
        track_name: Vec<u8>,
        parameters: MessageParameters,
    ) -> Result<FetchResult, BoxError> {
        let request_id = {
            let mut session = lock_session(&self.session);
            session
                .send_fetch(namespace, track_name, parameters)
                .map_err(|e| MoqError::Session(format!("send_fetch: {e}")))?
        };
        self.drain_events().await?;
        self.wait_fetch_resolved(request_id).await?;
        tracing::info!(target: "moq", request_id = request_id, "FETCH_OK received");
        Ok(FetchResult { request_id })
    }

    /// 購読が確立または拒否されるまでイベントを回す
    async fn wait_subscription_resolved(&mut self, request_id: u64) -> Result<(), BoxError> {
        loop {
            let state = {
                let session = lock_session(&self.session);
                session.subscription(request_id).map(|sub| sub.state)
            };
            match state {
                Some(SubscriptionState::Established) => return Ok(()),
                Some(SubscriptionState::Terminated) => {
                    return Err(
                        MoqError::Session(format!("request {request_id} was rejected")).into(),
                    );
                }
                _ => {}
            }
            self.pump_once().await?;
        }
    }

    /// FETCH が確立または拒否されるまでイベントを回す
    async fn wait_fetch_resolved(&mut self, request_id: u64) -> Result<(), BoxError> {
        loop {
            let state = {
                let session = lock_session(&self.session);
                session.fetch(request_id).map(|fetch| fetch.state)
            };
            match state {
                Some(FetchState::Established) => return Ok(()),
                Some(FetchState::Terminated) => {
                    return Err(MoqError::Session(format!(
                        "FETCH was rejected (request_id={request_id})"
                    ))
                    .into());
                }
                _ => {}
            }
            self.pump_once().await?;
        }
    }

    /// 次のイベントを返す
    ///
    /// peer からの REQUEST_UPDATE を先に返し、次に要求、最後に session の注目イベントを
    /// 返す。REQUEST_UPDATE を待たせると相手の CONTROL_MESSAGE_TIMEOUT を招くため優先する。
    pub async fn next_event(&mut self) -> Result<Option<ClientEvent>, BoxError> {
        loop {
            if let Some(update) = self.incoming_updates.pop_front() {
                return Ok(Some(ClientEvent::RequestUpdate(update)));
            }
            if let Some(request) = self.incoming_requests.pop_front() {
                return Ok(Some(ClientEvent::Request(request)));
            }
            if let Some(event) = self.take_notable_event() {
                return Ok(Some(ClientEvent::Session(event)));
            }
            self.pump_once().await?;
        }
    }

    /// peer から届いた要求に SUBSCRIBE_OK で応答する (publisher 側)
    pub async fn send_subscribe_ok(
        &mut self,
        request_id: u64,
        track_alias: u64,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<(), BoxError> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_subscribe_ok(request_id, track_alias, parameters, track_properties)
                .map_err(|e| MoqError::Session(format!("send_subscribe_ok: {e}")))?;
        }
        self.drain_events().await
    }

    /// peer から届いた FETCH に FETCH_OK で応答する (publisher 側)
    pub async fn send_fetch_ok(
        &mut self,
        request_id: u64,
        end_of_track: u8,
        end_location: Location,
        parameters: MessageParameters,
        track_properties: TrackProperties,
    ) -> Result<(), BoxError> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_fetch_ok(
                    request_id,
                    end_of_track,
                    end_location,
                    parameters,
                    track_properties,
                )
                .map_err(|e| MoqError::Session(format!("send_fetch_ok: {e}")))?;
        }
        self.drain_events().await
    }

    /// peer から届いた REQUEST_UPDATE に REQUEST_OK で応答する
    ///
    /// draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) は REQUEST_OK または
    /// REQUEST_ERROR のいずれか 1 通を必ず返すと定める。
    pub async fn send_request_ok(&mut self, request_id: u64) -> Result<(), BoxError> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_request_ok(request_id, MessageParameters::new(), TrackProperties::new())
                .map_err(|e| MoqError::Session(format!("send_request_ok: {e}")))?;
        }
        self.drain_events().await
    }

    /// peer から届いた要求を REQUEST_ERROR で拒否する
    pub async fn send_request_error(
        &mut self,
        request_id: u64,
        error_code: u64,
        reason: &str,
    ) -> Result<(), BoxError> {
        {
            let mut session = lock_session(&self.session);
            let phrase = ReasonPhrase::new(reason)
                .map_err(|e| MoqError::Session(format!("invalid reason phrase: {e}")))?;
            session
                .send_request_error(request_id, error_code, 0, phrase, None)
                .map_err(|e| MoqError::Session(format!("send_request_error: {e}")))?;
        }
        self.drain_events().await
    }

    /// FETCH 応答ストリームを開き、Object を 1 件書いて FIN する (publisher 側)
    ///
    /// draft-ietf-moq-transport-21 §11.4.1 (Fetch Header): FETCH_OK を返した publisher は
    /// 要求の Request ID を載せた FETCH_HEADER で応答 stream を開く。
    pub async fn send_fetch_response(
        &mut self,
        request_id: u64,
        group_id: u64,
        object_id: u64,
        payload: &[u8],
        properties_data: Option<&[u8]>,
    ) -> Result<(), BoxError> {
        let data_plane = self.data_plane();
        let mut stream = self.handle.open_send_stream().await?;
        let stream_id = DataStreamId(stream.stream_id());
        data_plane.send_fetch_header(stream_id, request_id)?;

        let mut buf = Vec::new();
        buf.extend_from_slice(&FetchHeader { request_id }.encode());
        let object = FetchStreamEntry::Object(FetchStreamObject {
            group_id: Some(group_id),
            subgroup_id: FetchSubgroupIdMode::Zero,
            object_id: Some(object_id),
            // 最初の Object は prior Object を持たないため Publisher Priority を明示する
            // (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
            publisher_priority: Some(128),
            has_properties: properties_data.is_some(),
            is_datagram_origin: false,
            payload_length: payload.len() as u64,
        });
        object
            .encode(properties_data, FetchPriorContext::First, &mut buf)
            .map_err(|e| MoqError::Session(format!("failed to encode fetch object: {e}")))?;
        buf.extend_from_slice(payload);

        data_plane.send_fetch_object(stream_id)?;
        stream.send(Bytes::from(buf)).await?;
        stream.finish()?;
        data_plane.send_fetch_data_stream_closed(stream_id)?;
        Ok(())
    }

    /// `Session::tick` を呼び出して期限を進める
    ///
    /// `now_ms` は単調増加のミリ秒。SESSION のタイムアウト判定はこの呼び出しで進むため、
    /// 定期的に呼ぶ必要がある。
    pub fn tick(&mut self, now_ms: u64) {
        let mut session = lock_session(&self.session);
        session.tick(now_ms);
        drop(session);
        self.cleanup_closed_requests();
    }

    /// 購読を停止する (subscriber 側)
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
    /// 送信方向は RESET_STREAM、受信方向は STOP_SENDING で打ち切る。
    pub async fn stop_sending(&mut self, request_id: u64) -> Result<(), BoxError> {
        {
            let mut session = lock_session(&self.session);
            session
                .stop_sending(request_id)
                .map_err(|e| MoqError::Session(format!("stop_sending: {e}")))?;
        }
        self.drain_events().await?;
        let error_code = DataStreamResetReason::Cancelled.error_code();
        if let Some(mut send) = self.bidi_sends.remove(&request_id)
            && let Err(e) = send.reset(error_code)
        {
            tracing::warn!(target: "moq", request_id = request_id, "failed to reset request stream: {e}");
        }
        self.closed_request_streams.insert(request_id);
        self.request_stop_sending(request_id, error_code).await;
        Ok(())
    }

    /// bidi 受信タスクへ STOP_SENDING を指示する (失敗は警告のみ)
    async fn request_stop_sending(&mut self, request_id: u64, error_code: u64) {
        let Some(stop_tx) = self.bidi_stop_txs.remove(&request_id) else {
            tracing::warn!(target: "moq", request_id = request_id, "no bidi receive task for STOP_SENDING");
            return;
        };
        let (ack_tx, ack_rx) = oneshot::channel();
        let command = StopSendingCommand {
            error_code,
            ack: ack_tx,
        };
        if stop_tx.send(command).await.is_err() {
            tracing::warn!(target: "moq", request_id = request_id, "bidi receive task already finished");
            return;
        }
        // 受信タスクが送信で詰まっている場合に備え、ack 待ちに上限を設ける
        match tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx).await {
            Ok(Ok(Ok(()))) => {
                tracing::info!(target: "moq", request_id = request_id, "sent STOP_SENDING");
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(target: "moq", request_id = request_id, "failed to send STOP_SENDING: {e}");
            }
            Ok(Err(_)) => {
                tracing::warn!(target: "moq", request_id = request_id, "bidi receive task dropped the command");
            }
            Err(_) => {
                tracing::warn!(target: "moq", request_id = request_id, "timed out waiting for STOP_SENDING");
            }
        }
    }

    /// GOAWAY を送信する
    ///
    /// draft-ietf-moq-transport-21 §9.2 (GOAWAY)。graceful shutdown の開始を通知する。
    pub async fn send_goaway(
        &mut self,
        new_session_uri: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<(), BoxError> {
        {
            let mut session = lock_session(&self.session);
            session
                .send_goaway(new_session_uri, timeout_ms)
                .map_err(|e| MoqError::Session(format!("send_goaway: {e}")))?;
        }
        self.drain_events().await?;
        tracing::info!(target: "moq", timeout_ms = timeout_ms, "sent GOAWAY");
        Ok(())
    }

    /// 制御メッセージ応答待ちタイムアウト (ms) を設定する
    pub fn set_control_message_timeout_ms(&mut self, timeout_ms: Option<u64>) {
        let mut session = lock_session(&self.session);
        session.set_control_message_timeout_ms(timeout_ms);
    }

    /// data stream タイムアウト (ms) を設定する
    pub fn set_data_stream_timeout_ms(&mut self, timeout_ms: Option<u64>) {
        let mut session = lock_session(&self.session);
        session.set_data_stream_timeout_ms(timeout_ms);
    }

    /// セッションを閉じる
    pub async fn close(&mut self, code: u64, reason: &'static str) -> Result<(), BoxError> {
        self.closing = true;
        {
            let mut session = lock_session(&self.session);
            session.close(code, reason);
        }
        self.drain_events().await?;
        if code == 0 {
            tracing::info!(target: "moq", "session closed");
        } else {
            tracing::warn!(target: "moq", code = code, reason = reason, "session closed with error");
        }
        Ok(())
    }

    /// アプリに通知すべき SessionEvent を取り出す
    fn take_notable_event(&mut self) -> Option<SessionEvent> {
        if let Some(event) = self.notable_events.pop_front() {
            return Some(event);
        }
        loop {
            let event = {
                let mut session = lock_session(&self.session);
                session.poll_event()
            };
            let event = event?;
            if is_notable_event(&event) {
                return Some(event);
            }
        }
    }

    /// control / bidi チャネルのいずれか 1 件を受信して Session に流し、events を drain する
    async fn pump_once(&mut self) -> Result<(), BoxError> {
        tokio::select! {
            maybe = self.control_rx.recv() => {
                let Some(result) = maybe else {
                    return Err(MoqError::Session("control channel closed".to_owned()).into());
                };
                match result? {
                    StreamRead::Value(message) => {
                        let mut session = lock_session(&self.session);
                        if let Err(e) = session.recv_control(message) {
                            // 相手の制御ストリーム終端は CloseSession イベントとして届く場合がある
                            tracing::warn!(target: "moq", "failed to handle control message: {e}");
                        }
                    }
                    StreamRead::Closed(end) => {
                        let mut session = lock_session(&self.session);
                        let _ = session.recv_control_stream_closed(end);
                    }
                }
                self.drain_events().await?;
            }
            maybe = self.bidi_rx.recv() => {
                let Some(message) = maybe else {
                    return Err(MoqError::Session("bidi channel closed".to_owned()).into());
                };
                self.handle_bidi_message(message).await?;
            }
        }
        Ok(())
    }

    /// bidi 受信タスクから届いた 1 件を処理する
    async fn handle_bidi_message(&mut self, message: BidiMessage) -> Result<(), BoxError> {
        match message {
            BidiMessage::Response(request_id, result) => match result? {
                StreamRead::Value(message) => {
                    {
                        let mut session = lock_session(&self.session);
                        session
                            .recv_stream_message(request_id, message)
                            .map_err(|e| MoqError::Session(format!("recv_stream_message: {e}")))?;
                    }
                    self.drain_events().await?;
                }
                StreamRead::Closed(end) => {
                    // 回収済み request への遅延 close は Session が no-op で吸収するため、
                    // 回収対象として登録しない
                    let register_for_cleanup = {
                        let mut session = lock_session(&self.session);
                        let result = session.recv_request_stream_closed(request_id, end);
                        result.is_ok()
                            && (session.subscription(request_id).is_some()
                                || session.fetch(request_id).is_some())
                    };
                    if register_for_cleanup {
                        self.closed_request_streams.insert(request_id);
                    }
                    tracing::debug!(target: "moq", request_id = request_id, "request stream closed by peer");
                    self.drain_events().await?;
                    self.cleanup_closed_requests();
                }
            },
            BidiMessage::PeerRequest {
                request_id,
                message,
                send,
                stop_tx,
            } => {
                {
                    let mut session = lock_session(&self.session);
                    session
                        .recv_request(message.clone())
                        .map_err(|e| MoqError::Session(format!("recv_request: {e}")))?;
                }
                self.bidi_sends.insert(request_id, send);
                self.bidi_stop_txs.insert(request_id, stop_tx);
                tracing::info!(target: "moq", request_id = request_id, "received peer request");
                self.incoming_requests.push_back(IncomingRequest {
                    request_id,
                    message,
                });
                // 受理時に Session が自動応答 (REQUEST_ERROR 等) を積むことがある
                self.drain_events().await?;
            }
        }
        Ok(())
    }

    /// Session のイベントキューを drain し、I/O 操作に変換する
    async fn drain_events(&mut self) -> Result<(), BoxError> {
        loop {
            let event = {
                let mut session = lock_session(&self.session);
                session.poll_event()
            };
            let Some(event) = event else {
                break;
            };
            match event {
                SessionEvent::SendControl(message) => {
                    let encoded = message
                        .encode()
                        .map_err(|e| MoqError::Session(format!("failed to encode message: {e}")))?;
                    self.control_send.send(Bytes::from(encoded)).await?;
                }
                SessionEvent::SendRequest {
                    request_id,
                    message,
                } => {
                    let (mut send, recv) = self.handle.open_bidi_stream().await?;
                    let encoded = message
                        .encode()
                        .map_err(|e| MoqError::Session(format!("failed to encode request: {e}")))?;
                    send.send(Bytes::from(encoded)).await?;
                    self.bidi_sends.insert(request_id, send);
                    let (stop_tx, stop_rx) = mpsc::channel(1);
                    self.bidi_stop_txs.insert(request_id, stop_tx);
                    spawn_bidi_recv_task(
                        request_id,
                        ControlStream::new(recv),
                        self.bidi_tx.clone(),
                        stop_rx,
                    );
                }
                SessionEvent::SendOnStream {
                    request_id,
                    message,
                    fin,
                } => {
                    // 送信方向を FIN / RESET 済みの request では送信半が台帳に無いため no-op にする
                    if let Some(mut send) = self.bidi_sends.remove(&request_id) {
                        let encoded = message.encode().map_err(|e| {
                            MoqError::Session(format!("failed to encode response: {e}"))
                        })?;
                        send.send(Bytes::from(encoded)).await?;
                        if fin {
                            send.finish()?;
                        } else {
                            self.bidi_sends.insert(request_id, send);
                        }
                    } else {
                        tracing::warn!(
                            target: "moq",
                            request_id = request_id,
                            "dropping response on closed request stream"
                        );
                    }
                }
                SessionEvent::FinishRequestStream { request_id } => {
                    if let Some(mut send) = self.bidi_sends.remove(&request_id)
                        && let Err(e) = send.finish()
                    {
                        tracing::warn!(target: "moq", request_id = request_id, "failed to finish request stream: {e}");
                    }
                }
                SessionEvent::ResetRequestStream {
                    request_id,
                    error_code,
                } => {
                    if let Some(mut send) = self.bidi_sends.remove(&request_id)
                        && let Err(e) = send.reset(error_code)
                    {
                        tracing::warn!(target: "moq", request_id = request_id, "failed to reset request stream: {e}");
                    }
                }
                SessionEvent::StopSendingRequestStream {
                    request_id,
                    error_code,
                } => {
                    self.closed_request_streams.insert(request_id);
                    self.request_stop_sending(request_id, error_code).await;
                }
                SessionEvent::CloseSession(error) => {
                    self.handle.close(error.code, error.reason);
                    if self.closing {
                        // 自側からの close で発生したイベントはエラーではない
                        return Ok(());
                    }
                    return Err(close_session_event_to_error(error).into());
                }
                SessionEvent::RequestUpdateReceived {
                    request_id,
                    parameters: _,
                } => {
                    self.incoming_updates
                        .push_back(IncomingRequestUpdate { request_id });
                }
                event @ (SessionEvent::GoawayReceived { .. }
                | SessionEvent::PublishDoneReceived { .. }) => {
                    self.notable_events.push_back(event);
                }
                SessionEvent::Established
                | SessionEvent::RequestErrorReceived { .. }
                | SessionEvent::RequestTerminated { .. }
                | SessionEvent::RequestOkReceived { .. }
                | SessionEvent::PublishStateNotifyReceived { .. }
                | SessionEvent::ResetDataStream { .. }
                | SessionEvent::FetchOkReceived { .. }
                | SessionEvent::SendPaddingStream { .. }
                | SessionEvent::OpenFillFetchStream { .. }
                | SessionEvent::SendPaddingDatagram { .. } => {
                    // 応答の成否は subscription / fetch の状態で判断するため、
                    // ここでは個別の処理を行わない
                }
            }
        }
        self.cleanup_closed_requests();
        Ok(())
    }

    /// 終端済み request stream を回収する
    fn cleanup_closed_requests(&mut self) {
        let request_ids: Vec<u64> = self.closed_request_streams.iter().copied().collect();
        let mut forgotten = Vec::new();
        {
            let mut session = lock_session(&self.session);
            for request_id in request_ids {
                if session.subscription_cleanup_ready(request_id) == Some(true)
                    && session.forget_subscription(request_id).is_some()
                {
                    forgotten.push(request_id);
                    continue;
                }
                if session.fetch_cleanup_ready(request_id) == Some(true)
                    && session.forget_fetch(request_id).is_some()
                {
                    forgotten.push(request_id);
                }
            }
        }
        for request_id in forgotten {
            self.closed_request_streams.remove(&request_id);
            self.bidi_stop_txs.remove(&request_id);
            if let Some(mut send) = self.bidi_sends.remove(&request_id) {
                let _ = send.finish();
            }
        }
    }
}

/// `CloseSession` イベントをエラーに変換する
fn close_session_event_to_error(error: MoqtSessionError) -> MoqError {
    let reason = error.reason;
    MoqError::Session(format!(
        "session closed: code={:#x} reason={reason}",
        error.code
    ))
}

/// 制御ストリームの受信タスクを起動する
fn spawn_control_recv_task(
    mut stream: ControlStream,
) -> (mpsc::Receiver<ControlIncoming>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<ControlIncoming>(16);
    let handle = tokio::spawn(async move {
        loop {
            match stream.recv_message().await {
                Ok(message) => {
                    let is_closed = matches!(message, StreamRead::Closed(_));
                    if tx.send(Ok(message)).await.is_err() || is_closed {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });
    (rx, handle)
}

/// bidi request stream の受信タスクを起動する
///
/// メインループから停止指示 (`mpsc`) を受けると、受信半に STOP_SENDING を送出し、
/// その結果を `ack` で返して終了する。
fn spawn_bidi_recv_task(
    request_id: u64,
    mut stream: ControlStream,
    tx: mpsc::Sender<BidiMessage>,
    mut stop_rx: mpsc::Receiver<StopSendingCommand>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                command = stop_rx.recv() => {
                    if let Some(command) = command {
                        let result = stream.stop_sending(command.error_code);
                        let _ = command.ack.send(result);
                    }
                    break;
                }
                message = stream.recv_message() => {
                    match message {
                        Ok(message) => {
                            let is_closed = matches!(message, StreamRead::Closed(_));
                            if tx
                                .send(BidiMessage::Response(request_id, Ok(message)))
                                .await
                                .is_err()
                                || is_closed
                            {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(BidiMessage::Response(request_id, Err(e))).await;
                            break;
                        }
                    }
                }
            }
        }
    });
}

/// peer が開始する bidi request stream を受け入れるタスクを起動する
///
/// relay は subscriber の SUBSCRIBE / FETCH を publisher 側の新しい request stream として
/// 転送するため、publisher はこのストリームを受理する必要がある。
fn spawn_peer_bidi_accept_task(
    mut acceptor: BidiStreamAcceptor,
    tx: mpsc::Sender<BidiMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let pair = match acceptor.accept_bidi_stream().await {
                Ok(Some(pair)) => pair,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(target: "moq", "failed to accept peer bidi stream: {e}");
                    break;
                }
            };
            let (send, recv) = pair;
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut stream = ControlStream::new(recv);
                // 先頭メッセージが request の開始メッセージである
                let first = match stream.recv_message().await {
                    Ok(StreamRead::Value(message)) => message,
                    Ok(StreamRead::Closed(_)) => return,
                    Err(e) => {
                        tracing::warn!(target: "moq", "failed to read peer request: {e}");
                        return;
                    }
                };
                let Some(request_id) = request_id_of(&first) else {
                    tracing::warn!(target: "moq", "peer request start message is not a request: {first:?}");
                    return;
                };
                let (stop_tx, mut stop_rx) = mpsc::channel::<StopSendingCommand>(1);
                if tx
                    .send(BidiMessage::PeerRequest {
                        request_id,
                        message: first,
                        send,
                        stop_tx,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    tokio::select! {
                        biased;
                        command = stop_rx.recv() => {
                            if let Some(command) = command {
                                let result = stream.stop_sending(command.error_code);
                                let _ = command.ack.send(result);
                            }
                            break;
                        }
                        message = stream.recv_message() => {
                            match message {
                                Ok(message) => {
                                    let is_closed = matches!(message, StreamRead::Closed(_));
                                    if tx
                                        .send(BidiMessage::Response(request_id, Ok(message)))
                                        .await
                                        .is_err()
                                        || is_closed
                                    {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(BidiMessage::Response(request_id, Err(e))).await;
                                    break;
                                }
                            }
                        }
                    }
                }
            });
        }
    })
}
