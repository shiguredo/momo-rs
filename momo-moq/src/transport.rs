//! MOQT の QUIC トランスポート
//!
//! `tobi-quic` のストリーム操作を MOQT から使う形に薄くラップする。
//! ストリームの役割は draft-ietf-moq-transport-21 §6.3 (Session initialization) /
//! §6.4 (Streams) に従う。draft 由来のため将来の改版で変わる可能性がある。

use std::sync::Arc;

use bytes::Bytes;
use shiguredo_moqt::session::types::RequestStreamEnd;
use tobi_quic::{
    Connection, ReceiveStream, SendStream as TobiSendStream, StreamAcceptor as TobiStreamAcceptor,
};

use crate::error::MoqError;

/// 受信 1 回あたりに取り出すデータ長の上限
///
/// `ReceiveStream::receive_vec` のバッファ長として使う。1 回で取り切れない
/// 場合は次の呼び出しで続きが返るため、MOQT のオブジェクト長より小さくてよい。
const RECV_BUFFER_SIZE: usize = 64 * 1024;

/// QUIC の可変長整数で表せる最大値 (RFC 9000 §16)
///
/// application error code はこの範囲に限られる。
const MAX_VARINT: u64 = (1 << 62) - 1;

/// 受信ストリームから取り出した 1 要素
pub enum RecvChunk {
    /// 受信データ
    Data(Bytes),
    /// ストリームの終端
    End(RequestStreamEnd),
}

/// 送信ストリーム
pub struct SendStream {
    stream: TobiSendStream,
}

impl SendStream {
    /// 送信ストリームをラップする
    pub fn new(stream: TobiSendStream) -> Self {
        Self { stream }
    }

    /// データを送信する
    ///
    /// フロー制御で一部しか受理されない場合は残りを再送し、全量を送り切るまで待つ。
    pub async fn send(&mut self, data: Bytes) -> Result<(), MoqError> {
        self.stream
            .send_all(&data)
            .await
            .map_err(|e| MoqError::Quic(format!("failed to send stream data: {e}")))
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        self.stream.id()
    }

    /// ストリームの送信方向を FIN で閉じる
    pub async fn finish(&mut self) -> Result<(), MoqError> {
        self.stream
            .finish()
            .await
            .map_err(|e| MoqError::Quic(format!("failed to finish stream: {e}")))
    }

    /// ストリームの送信方向を RESET_STREAM で打ち切る
    ///
    /// `error_code` は MOQT の Stream Reset Error Code
    /// (draft-ietf-moq-transport-21 §12.5) を QUIC の application error に載せる。
    pub async fn reset(&mut self, error_code: u64) -> Result<(), MoqError> {
        self.stream
            .reset_stream(error_code)
            .await
            .map_err(|e| MoqError::Quic(format!("failed to reset stream: {e}")))
    }
}

/// 受信ストリーム
pub struct RecvStream {
    stream: ReceiveStream,
}

impl RecvStream {
    /// 受信ストリームをラップする
    pub fn new(stream: ReceiveStream) -> Self {
        Self { stream }
    }

    /// データまたは終端を受信する
    ///
    /// FIN は [`RequestStreamEnd::Fin`]、RESET_STREAM / RESET_STREAM_AT は
    /// [`RequestStreamEnd::Reset`] として返す。FIN とデータが同じ読み取りで返る
    /// ことがあるため、データを先に返し、FIN は次の呼び出しで返す。
    pub async fn receive_chunk(&mut self) -> Result<RecvChunk, MoqError> {
        match self.stream.receive_vec(RECV_BUFFER_SIZE).await {
            Ok(chunk) => {
                if !chunk.data.is_empty() {
                    Ok(RecvChunk::Data(Bytes::from(chunk.data)))
                } else if chunk.fin {
                    Ok(RecvChunk::End(RequestStreamEnd::Fin))
                } else {
                    // データも FIN も無い読み取りは、バッファ長が 0 のとき以外は
                    // tobi-quic の契約上起きない
                    Err(MoqError::Quic(
                        "the stream returned neither data nor FIN".to_owned(),
                    ))
                }
            }
            Err(tobi_quic::Error::Stream(tobi_quic::StreamError::Reset {
                error_code,
                reliable_size,
                ..
            })) => {
                // QUIC 経路は QUIC のコード空間であり常にコードがあるため Some にする
                // (moqt-rs は WebTransport で MOQT のコードへ戻せない場合に None を使う)
                Ok(RecvChunk::End(RequestStreamEnd::Reset {
                    error_code: Some(error_code),
                    reliable_size,
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
    pub async fn stop_sending(&mut self, error_code: u64) -> Result<(), MoqError> {
        self.stream
            .stop_sending(error_code)
            .await
            .map_err(|e| MoqError::Quic(format!("failed to send STOP_SENDING: {e}")))
    }

    /// ストリーム ID を返す
    pub fn stream_id(&self) -> u64 {
        self.stream.id()
    }
}

/// 送信ストリームを開き datagram を送るためのハンドル
///
/// 複数の非同期タスクから clone して使う。`tobi_quic::Connection` は
/// コマンドハンドルであり、clone しても同じ接続を指す。
#[derive(Clone)]
pub struct StreamHandle {
    handle: Connection,
}

impl StreamHandle {
    /// 接続ハンドルをラップする
    pub fn new(handle: Connection) -> Self {
        Self { handle }
    }

    /// 単方向ストリームを開く
    ///
    /// ピアが許可したストリーム数の上限に達している場合は、ピアが上限を
    /// 増やすまで待つ。
    pub async fn open_send_stream(&self) -> Result<SendStream, MoqError> {
        let stream = self
            .handle
            .open_send_stream_wait()
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
            .open_bidirectional_stream_wait()
            .await
            .map_err(|e| MoqError::Quic(format!("failed to open bidi stream: {e}")))?;
        let (send, recv) = stream.split();
        Ok((SendStream::new(send), RecvStream::new(recv)))
    }

    /// 接続をクローズする
    ///
    /// QUIC の application error code として MOQT のセッションエラーコードを載せる。
    /// MOQT のコードは §13 (Grease) の greasing 値を含み u64 全域を取り得るが、
    /// QUIC の application error code は 62 ビット varint に限られるため、
    /// 収まらないコードは UNKNOWN (0) で閉じる。
    pub async fn close(&self, code: u64, reason: &str) {
        // 62 ビット varint に収まらないコードは UNKNOWN (0) に丸める
        let close_code = if code <= MAX_VARINT { code } else { 0 };
        tracing::info!(target: "moq", code = close_code, reason = reason, "closing QUIC connection");
        if close_code != code {
            tracing::warn!(
                target: "moq",
                code = code,
                "QUIC application error code is out of range; closing with UNKNOWN"
            );
        }
        if let Err(e) = self
            .handle
            .close_application(close_code, reason.as_bytes())
            .await
        {
            tracing::warn!(target: "moq", "failed to close QUIC connection: {e}");
        }
    }
}

/// ピアが開始したストリームを受け入れるアクセプター
///
/// `tobi_quic::StreamAcceptor` は単方向と双方向の両方を受け付けられる。
/// 受理は `&self` を取るため、単方向 (data stream) を待つタスクと双方向
/// (request stream) を待つタスクで `Arc` を共有する。
#[derive(Clone)]
pub struct StreamAcceptor {
    acceptor: Arc<TobiStreamAcceptor>,
}

impl StreamAcceptor {
    /// アクセプターをラップする
    pub fn new(acceptor: TobiStreamAcceptor) -> Self {
        Self {
            acceptor: Arc::new(acceptor),
        }
    }

    /// 単方向ストリームを 1 つ受け入れる
    ///
    /// 接続が閉じた場合は `Ok(None)` を返す。
    pub async fn accept_recv_stream(&self) -> Result<Option<RecvStream>, MoqError> {
        match self.acceptor.accept_receive_stream().await {
            Ok(stream) => Ok(Some(RecvStream::new(stream))),
            Err(tobi_quic::Error::Connection(_)) => Ok(None),
            Err(e) => Err(MoqError::Quic(format!("failed to accept data stream: {e}"))),
        }
    }

    /// 双方向ストリームを 1 つ受け入れる
    ///
    /// 接続が閉じた場合は `Ok(None)` を返す。
    pub async fn accept_bidi_stream(&self) -> Result<Option<(SendStream, RecvStream)>, MoqError> {
        match self.acceptor.accept_bidirectional_stream().await {
            Ok(stream) => {
                let (send, recv) = stream.split();
                Ok(Some((SendStream::new(send), RecvStream::new(recv))))
            }
            Err(tobi_quic::Error::Connection(_)) => Ok(None),
            Err(e) => Err(MoqError::Quic(format!("failed to accept bidi stream: {e}"))),
        }
    }
}
