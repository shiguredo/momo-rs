//! MOQT の QUIC トランスポート
//!
//! `s2n-quic` のストリーム操作を MOQT から使う形に薄くラップする。
//! ストリームの役割は draft-ietf-moq-transport-21 §6.3 (Session initialization) /
//! §6.4 (Streams) に従う。draft 由来のため将来の改版で変わる可能性がある。

use bytes::Bytes;
use shiguredo_moqt::session::types::RequestStreamEnd;

use crate::moq::error::MoqError;

/// 受信ストリームから取り出した 1 要素
pub enum RecvChunk {
    /// 受信データ
    Data(Bytes),
    /// ストリームの終端
    End(RequestStreamEnd),
}

/// 送信ストリーム
pub struct SendStream {
    stream: s2n_quic::stream::SendStream,
}

impl SendStream {
    /// 送信ストリームをラップする
    pub fn new(stream: s2n_quic::stream::SendStream) -> Self {
        Self { stream }
    }

    /// データを送信する
    pub async fn send(&mut self, data: Bytes) -> Result<(), MoqError> {
        self.stream
            .send(data)
            .await
            .map_err(|e| MoqError::Quic(format!("failed to send stream data: {e}")))
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        self.stream.id()
    }

    /// ストリームの送信方向を FIN で閉じる
    pub fn finish(&mut self) -> Result<(), MoqError> {
        self.stream
            .finish()
            .map_err(|e| MoqError::Quic(format!("failed to finish stream: {e}")))
    }

    /// ストリームの送信方向を RESET_STREAM で打ち切る
    ///
    /// `error_code` は MOQT の Stream Reset Error Code
    /// (draft-ietf-moq-transport-21 §12.5) を QUIC の application error に載せる。
    pub fn reset(&mut self, error_code: u64) -> Result<(), MoqError> {
        let code = s2n_quic::application::Error::new(error_code)
            .map_err(|e| MoqError::Quic(format!("invalid reset error code {error_code}: {e}")))?;
        self.stream
            .reset(code)
            .map_err(|e| MoqError::Quic(format!("failed to reset stream: {e}")))
    }
}

/// 受信ストリーム
pub struct RecvStream {
    stream: s2n_quic::stream::ReceiveStream,
}

impl RecvStream {
    /// 受信ストリームをラップする
    pub fn new(stream: s2n_quic::stream::ReceiveStream) -> Self {
        Self { stream }
    }

    /// データまたは終端を受信する
    ///
    /// FIN は [`RequestStreamEnd::Fin`]、RESET_STREAM は [`RequestStreamEnd::Reset`] として返す。
    pub async fn receive_chunk(&mut self) -> Result<RecvChunk, MoqError> {
        match self.stream.receive().await {
            Ok(Some(data)) => Ok(RecvChunk::Data(data)),
            Ok(None) => Ok(RecvChunk::End(RequestStreamEnd::Fin)),
            Err(s2n_quic::stream::Error::StreamReset { error, .. }) => {
                Ok(RecvChunk::End(RequestStreamEnd::Reset {
                    error_code: error.into(),
                    reliable_size: None,
                }))
            }
            Err(e) => Err(MoqError::Quic(format!(
                "failed to receive stream data: {e}"
            ))),
        }
    }

    /// 受信方向へ STOP_SENDING を送出する
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
    /// 受信方向の cancel は STOP_SENDING で行う。draft 由来のため将来の改版で変わる可能性がある。
    pub fn stop_sending(&mut self, error_code: u64) -> Result<(), MoqError> {
        let code = s2n_quic::application::Error::new(error_code)
            .map_err(|e| MoqError::Quic(format!("invalid stop sending code {error_code}: {e}")))?;
        self.stream
            .stop_sending(code)
            .map_err(|e| MoqError::Quic(format!("failed to send STOP_SENDING: {e}")))
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        self.stream.id()
    }
}

/// 送信ストリームを開き datagram を送るためのハンドル
///
/// 複数の非同期タスクから clone して使う。
#[derive(Clone)]
pub struct StreamHandle {
    handle: s2n_quic::connection::Handle,
}

impl StreamHandle {
    /// 接続ハンドルをラップする
    pub fn new(handle: s2n_quic::connection::Handle) -> Self {
        Self { handle }
    }

    /// 単方向ストリームを開く
    pub async fn open_send_stream(&self) -> Result<SendStream, MoqError> {
        let stream = self
            .handle
            .clone()
            .open_send_stream()
            .await
            .map_err(|e| MoqError::Quic(format!("failed to open send stream: {e}")))?;
        Ok(SendStream::new(stream))
    }

    /// 双方向ストリームを開く
    ///
    /// draft-ietf-moq-transport-21 §6.3 (Session initialization): PUBLISH / SUBSCRIBE /
    /// FETCH の要求メッセージは双方向ストリームで送受信する。
    pub async fn open_bidi_stream(&self) -> Result<(SendStream, RecvStream), MoqError> {
        let stream = self
            .handle
            .clone()
            .open_bidirectional_stream()
            .await
            .map_err(|e| MoqError::Quic(format!("failed to open bidi stream: {e}")))?;
        let (recv, send) = stream.split();
        Ok((SendStream::new(send), RecvStream::new(recv)))
    }

    /// 接続をクローズする
    ///
    /// QUIC の application error code として MOQT のセッションエラーコードを載せる。
    pub fn close(&self, code: u64, reason: &str) {
        let error = s2n_quic::application::Error::new(code)
            .unwrap_or(s2n_quic::application::Error::UNKNOWN);
        tracing::info!(target: "moq", code = code, reason = reason, "closing QUIC connection");
        self.handle.close(error);
    }
}

/// 単方向ストリーム (data stream) を受け入れるアクセプター
pub struct StreamAcceptor {
    acceptor: s2n_quic::connection::ReceiveStreamAcceptor,
}

impl StreamAcceptor {
    /// アクセプターをラップする
    pub fn new(acceptor: s2n_quic::connection::ReceiveStreamAcceptor) -> Self {
        Self { acceptor }
    }

    /// 単方向ストリームを 1 つ受け入れる
    ///
    /// 接続が閉じた場合は `Ok(None)` を返す。
    pub async fn accept_recv_stream(&mut self) -> Result<Option<RecvStream>, MoqError> {
        let stream = self
            .acceptor
            .accept_receive_stream()
            .await
            .map_err(|e| MoqError::Quic(format!("failed to accept data stream: {e}")))?;
        Ok(stream.map(RecvStream::new))
    }
}

/// peer (MOQT relay) が開始する双方向ストリームを受け入れるアクセプター
///
/// relay は subscriber の SUBSCRIBE / FETCH を publisher 側の新しい request stream として
/// 転送するため、publisher はこのストリームを受理しなければ要求に応答できない。
pub struct BidiStreamAcceptor {
    acceptor: s2n_quic::connection::BidirectionalStreamAcceptor,
}

impl BidiStreamAcceptor {
    /// アクセプターをラップする
    pub fn new(acceptor: s2n_quic::connection::BidirectionalStreamAcceptor) -> Self {
        Self { acceptor }
    }

    /// 双方向ストリームを 1 つ受け入れる
    ///
    /// 接続が閉じた場合は `Ok(None)` を返す。
    pub async fn accept_bidi_stream(
        &mut self,
    ) -> Result<Option<(SendStream, RecvStream)>, MoqError> {
        let stream = self
            .acceptor
            .accept_bidirectional_stream()
            .await
            .map_err(|e| MoqError::Quic(format!("failed to accept bidi stream: {e}")))?;
        Ok(stream.map(|stream| {
            let (recv, send) = stream.split();
            (SendStream::new(send), RecvStream::new(recv))
        }))
    }
}
