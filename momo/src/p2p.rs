//! P2P モードの実装
//!
//! TCP リスナー + HTTP 静的ファイルサーバー + WebSocket シグナリング + WebRTC

mod http;
mod message;
mod signaling;
mod webrtc;
mod websocket;

use std::path::PathBuf;
use std::sync::Arc;

use shiguredo_http11::{HttpHead, RequestDecoder};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info};

use crate::error::BoxError;
use crate::metrics::MetricsState;

/// P2P サーバーの設定
pub struct P2PConfig {
    /// 待ち受けポート番号 (`--port`)
    pub port: u16,
    /// 静的ファイルを配信するドキュメントルート (`--document-root`)
    pub document_root: PathBuf,
    /// Google の STUN サーバーを使わない (`--no-google-stun`)
    pub no_google_stun: bool,
    /// 音声デバイスを使わない (`--no-audio-device`)
    pub no_audio_device: bool,
    /// 映像入力デバイスを使わない (`--no-video-input-device`)
    pub no_video_input_device: bool,
    /// 実デバイスの代わりにフェイクのキャプチャを使う (`--fake-capture-device`)
    pub fake_capture_device: bool,
    /// libcamera でキャプチャする (`--use-libcamera`、Raspberry Pi feature のみ)
    pub use_libcamera: bool,
    /// libcamera の DMA-BUF を V4L2 エンコーダーへ直接渡す (`--use-libcamera-native`)
    #[cfg_attr(
        not(feature = "raspberrypi"),
        expect(dead_code, reason = "raspberrypi feature が無効なビルドでは読まない")
    )]
    pub use_libcamera_native: bool,
    /// libcamera のコントロール設定 (`--libcamera-control` のキーと値)
    #[cfg_attr(
        not(feature = "raspberrypi"),
        expect(dead_code, reason = "raspberrypi feature が無効なビルドでは読まない")
    )]
    pub libcamera_controls: Vec<(String, String)>,
    /// V4L2 M2M のハードウェア H.264 エンコーダーを使う (`--use-v4l2-encoder`、Raspberry Pi feature のみ)
    pub use_v4l2_encoder: bool,
    /// 読み込み済みの OpenH264 ライブラリ (`--openh264` 指定時のみ `Some`)
    pub openh264_lib: Option<shiguredo_openh264::Openh264Library>,
    /// `--{codec}-encoder` / `--{codec}-decoder` によるコーデック選択
    pub codec_selection: crate::video_codec_factory::CodecSelection,
    /// 映像入力デバイスの指定 (`--video-input-device`、インデックス番号かデバイス名)
    pub video_input_device: Option<String>,
    /// 音声入力デバイスの指定 (`--audio-input-device`、インデックス番号かデバイス名)
    pub audio_input_device: Option<String>,
    /// `--audio-output-device` で指定されたデバイス (インデックス番号・デバイス名・unique_id)
    pub audio_output_device: Option<String>,
    /// 映像の幅 (ピクセル)
    pub video_width: i32,
    /// 映像の高さ (ピクセル)
    pub video_height: i32,
    /// 映像のフレームレート (fps)
    pub framerate: u32,
    /// 映像入力のピクセル形式を固定する (`--force-i420` など。`None` なら自動選択)
    pub force_pixel_format: Option<shiguredo_video_device::PixelFormat>,
    /// 映像劣化時に優先する品質 (`--degradation-preference`)
    pub degradation_preference: shiguredo_webrtc::DegradationPreference,
    /// 映像コーデック種別の指定 (`--video-codec-type`)
    pub video_codec_type: Option<String>,
    /// 音声コーデック種別の指定 (`--audio-codec-type`)
    pub audio_codec_type: Option<String>,
    /// シリアル経由の DataChannel パススルー設定 (`--serial`)
    #[cfg(target_os = "linux")]
    pub serial: Option<crate::serial::SerialConfig>,
    /// メトリクスサーバーと共有する状態 (無効時は `None`)
    pub metrics_state: Option<Arc<MetricsState>>,
}

/// P2P サーバーを起動する
pub async fn run(config: P2PConfig) -> Result<(), BoxError> {
    let config = Arc::new(config);
    let engine = Arc::new(webrtc::WebRtcEngine::new(&config)?);
    let listener = TcpListener::bind(("0.0.0.0", config.port)).await?;
    info!(target: "p2p", port = config.port, "server started");

    loop {
        let (socket, addr) = listener.accept().await?;
        let engine = engine.clone();
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(socket, engine, config).await {
                error!(target: "p2p", src_addr = %addr, error = %e, "connection error");
            }
        });
    }
}

/// HTTP リクエストを解析し、/ws なら WebSocket へ、それ以外は静的ファイルへ振り分ける
async fn handle_connection(
    mut socket: TcpStream,
    engine: Arc<webrtc::WebRtcEngine>,
    config: Arc<P2PConfig>,
) -> Result<(), BoxError> {
    let mut decoder = RequestDecoder::new();
    let mut raw_bytes: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 4096];

    let head = loop {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        raw_bytes.extend_from_slice(&buf[..n]);
        decoder.feed(&buf[..n])?;
        if let Some((head, _)) = decoder.decode_headers()? {
            break head;
        }
    };

    let path = head
        .uri()
        .split('?')
        .next()
        .unwrap_or(head.uri())
        .to_owned();
    info!(target: "p2p", method = head.method(), %path, src_addr = %socket.peer_addr().map(|a| a.to_string()).unwrap_or_default(), "HTTP request");

    if head.method() == "GET" && path == "/ws" {
        let is_upgrade = head
            .get_header("Upgrade")
            .map(|v| v.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
        if is_upgrade {
            return websocket::handle_websocket(
                socket,
                raw_bytes,
                engine,
                config.clone(),
                config.metrics_state.clone(),
            )
            .await;
        }
    }

    http::serve_static_file(socket, head.method(), &path, &config.document_root).await
}
