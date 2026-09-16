//! MOQT (Media over QUIC Transport) モード
//!
//! `draft-ietf-moq-transport-21` に基づくクライアント機能を提供する。
//! Sans-I/O な `shiguredo_moqt` の `Session` と `s2n-quic` の I/O を繋ぐ層を
//! `moq/quic.rs` に隔離し、将来 QUIC 実装を差し替えられるようにする。

pub mod cli;
pub mod error;
pub mod quic;
pub mod url;

use std::time::Duration;

use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::parameter::{
    SETUP_OPTION_AUTHORITY, SETUP_OPTION_MOQT_IMPLEMENTATION, SETUP_OPTION_PATH, SetupOption,
    SetupOptionValue, SetupOptions,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{SessionEvent, SessionState, Transport};
use shiguredo_moqt::stream::encode_control_stream_setup;
use tracing::{info, warn};

use crate::error::BoxError;
use error::MoqError;
use url::ServerUrl;

/// MOQT セッション確立までの待ち時間の上限
///
/// 相手が SETUP を返さない場合に無限に待たないようにする。
const ESTABLISH_TIMEOUT: Duration = Duration::from_secs(10);

/// SETUP の MOQT_IMPLEMENTATION に載せる実装名
const IMPLEMENTATION_NAME: &str = "momo-rs";

/// MOQT モードの設定
#[derive(Debug, Clone)]
pub struct MoqConfig {
    /// 接続先 URL (`moqt://host:port/path`)
    pub url: String,
    /// サーバー証明書の検証をスキップするかどうか (開発用)
    pub insecure: bool,
    /// CA 証明書のパス。指定した場合はこの証明書で検証する
    pub ca_cert: Option<String>,
}

/// MOQT セッションの制御ストリーム
///
/// MOQT は制御メッセージ用に単方向ストリームを 2 本使う。自身が書き込む側と、
/// 相手が書き込む側をまとめて保持する。セッションを維持する間はドロップしないこと。
pub struct ControlStream {
    /// 自身が制御メッセージを書き込むストリーム
    send: s2n_quic::stream::SendStream,
    /// 相手の制御メッセージを読み出すリーダー
    pub reader: ControlStreamReader,
}

impl ControlStream {
    /// 制御メッセージを 1 つ送信する
    ///
    /// メッセージにはストリームタイプを付けない。ストリームタイプは制御ストリームの
    /// 先頭にだけ付く (draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams))。
    pub async fn send(&mut self, message: &ControlMessage) -> Result<(), BoxError> {
        let encoded = message
            .encode()
            .map_err(|e| MoqError::Session(format!("failed to encode message: {e}")))?;
        self.send
            .send(encoded.into())
            .await
            .map_err(|e| MoqError::Quic(format!("failed to send message: {e}")))?;
        Ok(())
    }
}

/// MOQT セッションを確立する
///
/// `draft-ietf-moq-transport-21` §6.2 (Session establishment) / §6.3 (Session initialization)
/// に従い、QUIC 接続の確立、制御ストリームの開設、SETUP の交換までを行う。
/// 戻り値の `Session` は `Established` 状態になっている。
///
/// 相手が制御ストリームを開かない、または SETUP を返さない場合は
/// [`ESTABLISH_TIMEOUT`] でタイムアウトする。
pub async fn establish_session(config: &MoqConfig) -> Result<(Session, ControlStream), BoxError> {
    let server = ServerUrl::parse(&config.url)?;
    let connection = quic::connect(&server, config.insecure, config.ca_cert.as_deref()).await?;

    // 接続を handle (送信側) と acceptor (受信側) に分ける
    let (mut handle, acceptor) = connection.split();
    let (_bidi_acceptor, mut recv_acceptor) = acceptor.split();

    // 自側の制御ストリームを開く
    let mut send = handle
        .open_send_stream()
        .await
        .map_err(|e| MoqError::Quic(format!("failed to open control stream: {e}")))?;

    // Session を生成する。この時点で SETUP の送信イベントが積まれている
    let mut session = Session::new_client(Transport::Quic, build_setup_options(&server))
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
    let encoded =
        encode_control_stream_setup(&setup).map_err(|e| MoqError::Session(e.to_string()))?;
    send.send(encoded.into())
        .await
        .map_err(|e| MoqError::Quic(format!("failed to send SETUP: {e}")))?;
    info!(target: "moq", "sent SETUP message");

    // 相手の制御ストリームを待つ
    let recv_stream = match tokio::time::timeout(
        ESTABLISH_TIMEOUT,
        recv_acceptor.accept_receive_stream(),
    )
    .await
    {
        Ok(Ok(Some(stream))) => stream,
        Ok(Ok(None)) => {
            return Err(MoqError::Quic("connection closed before control stream".into()).into());
        }
        Ok(Err(e)) => {
            return Err(MoqError::Quic(format!("failed to accept control stream: {e}")).into());
        }
        Err(_) => {
            return Err(MoqError::Session(format!(
                "control stream was not opened within {ESTABLISH_TIMEOUT:?}"
            ))
            .into());
        }
    };

    let mut reader = ControlStreamReader::new(recv_stream);

    // 制御ストリームの先頭はストリームタイプの varint で、制御ストリームは 0x2F00 でなければならない
    // (draft-ietf-moq-transport-21 §6.4.1 (Unidirectional Streams))
    let stream_type = reader
        .read_varint(ESTABLISH_TIMEOUT)
        .await?
        .ok_or_else(|| MoqError::Quic("control stream closed before stream type".into()))?;
    session
        .recv_control_stream_type(stream_type)
        .map_err(|e| MoqError::Session(format!("unexpected control stream type: {e}")))?;

    // 相手の SETUP を待って Session に渡す
    let peer_setup = reader
        .read_message(ESTABLISH_TIMEOUT)
        .await?
        .ok_or_else(|| MoqError::Quic("control stream closed before SETUP".into()))?;
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
    info!(target: "moq", state = ?session.state(), "MOQT session established");

    // 確立時に積まれたイベントを処理する
    while let Some(event) = session.poll_event() {
        handle_session_event(event)?;
    }

    Ok((session, ControlStream { send, reader }))
}

/// SETUP 交換後に届くイベントを 1 つ処理する
///
/// `Established` は確立済みのため何もしない。`CloseSession` はエラーとして扱う。
/// それ以外は現時点では未対応のため警告ログを残す。
fn handle_session_event(event: SessionEvent) -> Result<(), BoxError> {
    match event {
        SessionEvent::Established => Ok(()),
        SessionEvent::CloseSession(reason) => {
            Err(MoqError::Session(format!("session closed: {reason:?}")).into())
        }
        other => {
            warn!(target: "moq", event = ?other, "unhandled session event");
            Ok(())
        }
    }
}

/// SETUP に載せる Setup Options を構築する
///
/// `draft-ietf-moq-transport-21` §9.1.1 (AUTHORITY) / §9.1.2 (PATH) は
/// QUIC 直接接続では authority と path を MUST で含めると定める。
/// §9.1.5 (MOQT_IMPLEMENTATION) は実装名を通知する任意のオプション。
fn build_setup_options(server: &ServerUrl) -> SetupOptions {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: SETUP_OPTION_AUTHORITY,
        value: SetupOptionValue::Bytes(server.authority().as_bytes().to_vec()),
    });
    // パスが指定されていない場合は PATH を載せない
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

/// 制御ストリームの受信をバッファリングして 1 メッセージずつ取り出すリーダー
///
/// QUIC のフレーム境界と MOQT のメッセージ境界は一致しないため、
/// 受信データを蓄積しながらインクリメンタルにデコードする。
pub struct ControlStreamReader {
    stream: s2n_quic::stream::ReceiveStream,
    decoder: MessageDecoder,
}

impl ControlStreamReader {
    /// 受信ストリームからリーダーを作る
    pub fn new(stream: s2n_quic::stream::ReceiveStream) -> Self {
        Self {
            stream,
            decoder: MessageDecoder::new(),
        }
    }

    /// ストリーム先頭の varint を 1 つ読む
    ///
    /// タイムアウトした場合とストリームが終端した場合は `Ok(None)` を返す。
    async fn read_varint(&mut self, timeout: Duration) -> Result<Option<u64>, BoxError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(value) = self
                .decoder
                .try_decode_varint()
                .map_err(|e| MoqError::Session(format!("failed to decode varint: {e}")))?
            {
                return Ok(Some(value));
            }
            let Some(data) = self.receive(deadline).await? else {
                return Ok(None);
            };
            self.decoder.push(&data);
        }
    }

    /// 制御メッセージを 1 つ読む
    ///
    /// タイムアウトした場合とストリームが終端した場合は `Ok(None)` を返す。
    pub async fn read_message(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ControlMessage>, BoxError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(message) = self
                .decoder
                .try_decode_message()
                .map_err(|e| MoqError::Session(format!("failed to decode message: {e}")))?
            {
                return Ok(Some(message));
            }
            let Some(data) = self.receive(deadline).await? else {
                return Ok(None);
            };
            self.decoder.push(&data);
        }
    }

    /// 制御ストリームから 1 チャンク受信する
    ///
    /// 期限までにデータが届かない場合とストリームが終端した場合は `Ok(None)` を返す。
    async fn receive(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<Option<bytes::Bytes>, BoxError> {
        match tokio::time::timeout_at(deadline, self.stream.receive()).await {
            Ok(Ok(Some(data))) => Ok(Some(data)),
            Ok(Ok(None)) => Ok(None),
            Ok(Err(e)) => Err(MoqError::Quic(format!("failed to receive: {e}")).into()),
            Err(_) => Ok(None),
        }
    }
}
