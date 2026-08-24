//! Sora モードの実装
//!
//! sora_sdk を使用して WebRTC SFU Sora に接続する。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use shiguredo_audio_device::{AudioCapture, AudioCaptureConfig};
use shiguredo_video_device::{VideoCapture, VideoCaptureConfig};
use shiguredo_webrtc::{
    AdaptFrameResult, AdaptedVideoTrackSource, AudioDeviceModule, I420Buffer, TimestampAligner,
};
#[cfg(feature = "amf")]
use sora_sdk::AmfVideoCodecCapability;
#[cfg(feature = "nvcodec")]
use sora_sdk::NvCodecVideoCodecCapability;
#[cfg(feature = "vpl")]
use sora_sdk::VplVideoCodecCapability;
use sora_sdk::{
    AdmConfig, Audio, CodecDirection, ConnectDataChannel, ForwardingFilter, JsonString, ProxyInfo,
    Role, SoraConnection, SoraConnectionContext, SoraConnectionContextConfig, Video,
    VideoCodecImplementation, VideoCodecPreference,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::adm::{AdmState, build_adm_handler};
use crate::error::BoxError;
use crate::fake::start_fake_video_thread;
use crate::metrics::MetricsState;

mod connect_options;
mod event_handler;

use connect_options::{build_data_channels, parse_forwarding_filters, validate_opaque_id};
use event_handler::SoraEventHandler;

// ─── 設定 ─────────────────────────────────────────────────────────────────────

// 映像入力デバイスや HW エンコーダー選択の各項目は、対応 feature を有効化したビルドでのみ
// 読み出される。このターゲット / feature 組み合わせでは未読の項目が残るため、
// デッドコード警告を抑制する。
#[expect(dead_code, reason = "feature 組み合わせによって未読の項目が残る")]
pub struct SoraConfig {
    pub signaling_urls: Vec<String>,
    pub channel_id: String,
    pub role: Role,
    pub video: bool,
    pub audio: bool,
    pub video_codec_type: Option<String>,
    pub audio_codec_type: Option<String>,
    pub video_bit_rate: u32,
    pub audio_bit_rate: u32,
    pub h264_encoder: Option<String>,
    pub h264_decoder: Option<String>,
    pub h265_encoder: Option<String>,
    pub h265_decoder: Option<String>,
    pub vp9_encoder: Option<String>,
    pub vp9_decoder: Option<String>,
    pub av1_encoder: Option<String>,
    pub av1_decoder: Option<String>,
    pub spotlight: bool,
    pub simulcast: bool,
    pub data_channel_signaling: Option<bool>,
    pub ignore_disconnect_websocket: Option<bool>,
    pub metadata: Option<String>,
    /// `--client-id` で指定された Sora に送信するクライアント ID
    pub client_id: Option<String>,
    /// `--bundle-id` で指定された Sora に送信するバンドル ID
    pub bundle_id: Option<String>,
    /// `--signaling-notify-metadata` で指定された JSON 文字列
    pub signaling_notify_metadata: Option<String>,
    /// `--data-channel-label` で指定された DataChannel ラベル (`#` 付き) の一覧
    pub data_channel_labels: Vec<String>,
    /// `--forwarding-filter` で指定された転送フィルターの JSON 文字列
    pub forwarding_filters: Option<String>,
    pub no_audio_device: bool,
    pub no_video_input_device: bool,
    pub fake_capture_device: bool,
    pub use_v4l2_encoder: bool,
    pub openh264_lib: Option<shiguredo_openh264::Openh264Library>,
    pub use_libcamera: bool,
    pub use_libcamera_native: bool,
    pub libcamera_controls: Vec<(String, String)>,
    pub video_input_device: Option<String>,
    pub audio_input_device: Option<String>,
    /// `--audio-output-device` で指定されたデバイス (インデックス番号・デバイス名・unique_id)
    pub audio_output_device: Option<String>,
    pub video_width: i32,
    pub video_height: i32,
    pub framerate: u32,
    pub disconnect_wait_timeout: u32,
    pub insecure: bool,
    pub force_pixel_format: Option<shiguredo_video_device::PixelFormat>,
    /// クライアント証明書 (cert_pem, key_pem)
    pub client_cert: Option<(String, String)>,
    /// CA 証明書 (PEM)
    pub ca_cert: Option<String>,
    /// プロキシ URL
    pub proxy_url: Option<String>,
    /// プロキシ認証ユーザー名
    pub proxy_username: Option<String>,
    /// プロキシ認証パスワード
    pub proxy_password: Option<String>,
    /// 受信映像表示 (player) のコマンド送信チャネル
    #[cfg(feature = "player")]
    pub display_cmd_tx: Option<std::sync::mpsc::Sender<crate::display::DisplayCommand>>,
}

// ─── 公開 API ─────────────────────────────────────────────────────────────────

/// 再接続の間隔の基準秒数
///
/// momo (C++) の watchdog と同じ `5 * (2 * retry + 1)` 秒 (5, 15, 25, ...) で再試行する。
const RECONNECT_BASE_SECONDS: u64 = 5;

/// 検証・変換を済ませた接続設定
///
/// 何度再接続しても結果は変わらないので、デバイスを開く前に 1 回だけ作る。
struct Prepared {
    video: Video,
    audio: Audio,
    metadata: Option<JsonString>,
    signaling_notify_metadata: Option<JsonString>,
    data_channels: Vec<ConnectDataChannel>,
    forwarding_filters: Vec<ForwardingFilter>,
}

/// Sora への接続を繰り返す
///
/// 接続が終了すれば切断されたとみなして再接続を続ける。設定そのものが不正なときに
/// はデバイスを開く前にエラーを返して終了する。`stop` が true になれば待機を打ち切って
/// 通常の終了とする (Ctrl+C 以外での停止経路)。
pub async fn run(
    config: SoraConfig,
    metrics_state: Option<Arc<MetricsState>>,
    stop: Arc<AtomicBool>,
) -> Result<(), BoxError> {
    let prepared = prepare(&config)?;
    let mut retry: u32 = 0;

    let connected = Arc::new(AtomicBool::new(false));

    loop {
        if stop.load(Ordering::Relaxed) {
            info!(target: "sora", "reconnect loop stopped");
            return Ok(());
        }

        match connect(&config, &prepared, metrics_state.as_ref(), &connected).await {
            Ok(()) => info!(target: "sora", "connection finished"),
            Err(error) => warn!(target: "sora", error = %error, "connection failed"),
        }

        // 接続を確立したうえでの終了だった場合は再接続回数を数え直す (momo と同じ)
        if connected.swap(false, Ordering::Relaxed) {
            retry = 0;
        }

        let wait_seconds = RECONNECT_BASE_SECONDS * (2 * u64::from(retry) + 1);
        retry = retry.saturating_add(1);
        info!(
            target: "sora",
            retry_seconds = wait_seconds,
            "reconnecting to Sora"
        );
        wait_before_retry(wait_seconds, &stop).await;
    }
}

/// 再接続までの待機
///
/// 停止要求を待機中でも受け付けられるように、短い間隔に分割して待つ。
async fn wait_before_retry(seconds: u64, stop: &AtomicBool) {
    let mut remaining = std::time::Duration::from_secs(seconds);
    while !remaining.is_zero() && !stop.load(Ordering::Relaxed) {
        let step = remaining.min(std::time::Duration::from_millis(500));
        tokio::time::sleep(step).await;
        remaining -= step;
    }
}

/// 接続設定を検証し、sora_sdk の型へ変換する
fn prepare(config: &SoraConfig) -> Result<Prepared, BoxError> {
    // デバイスを開いた後に不正な値で失敗すると、ユーザーには
    // 「つながったが直後に落ちた」ように見える。ここで先に弾む。
    if let Some(client_id) = config.client_id.as_deref() {
        validate_opaque_id(client_id, "--client-id")?;
    }
    if let Some(bundle_id) = config.bundle_id.as_deref() {
        validate_opaque_id(bundle_id, "--bundle-id")?;
    }
    // feature の組み合わせで決まる誤用も接続前に検出する
    #[cfg(not(feature = "raspberrypi"))]
    if config.use_libcamera {
        return Err("--use-libcamera は raspberrypi feature が有効な場合のみ使用できます".into());
    }

    Ok(Prepared {
        video: build_video(
            config.video,
            config.video_codec_type.as_deref(),
            config.video_bit_rate,
        )?,
        audio: build_audio(
            config.audio,
            config.audio_codec_type.as_deref(),
            config.audio_bit_rate,
        )?,
        metadata: config
            .metadata
            .as_deref()
            .map(parse_json_string)
            .transpose()?,
        signaling_notify_metadata: config
            .signaling_notify_metadata
            .as_deref()
            .map(parse_json_string)
            .transpose()?,
        data_channels: build_data_channels(&config.data_channel_labels)?,
        forwarding_filters: match config.forwarding_filters.as_deref() {
            Some(text) => parse_forwarding_filters(text)?,
            None => Vec::new(),
        },
    })
}

/// 1 回の Sora 接続を実行する
///
/// 呼び出し側 (`run`) は返り値のいかんにかかわらず再接続する。
async fn connect(
    config: &SoraConfig,
    prepared: &Prepared,
    metrics_state: Option<&Arc<MetricsState>>,
    connected: &Arc<AtomicBool>,
) -> Result<(), BoxError> {
    // フェイク映像とフェイク音声を同期させるトリガーは接続ごとに作り直す
    let beep_trigger = if config.fake_capture_device && !config.no_audio_device {
        Some(crate::fake::BeepTrigger::new())
    } else {
        None
    };

    // ── 音声デバイス構築 ──────────────────────────────────────────────────
    let AudioDevices {
        adm: adm_config,
        state: adm_state,
        capture: audio_capture,
        fake: _fake_audio_capturer,
    } = build_audio_devices(config, beep_trigger.as_ref())?;

    // ── SoraConnectionContext 生成 ────────────────────────────────────────
    // サイマルキャスト時はネイティブバッファを使用しない
    #[cfg(feature = "raspberrypi")]
    let dmabuf_map = if config.use_libcamera_native
        && config.use_libcamera
        && config.use_v4l2_encoder
        && !config.simulcast
    {
        Some(crate::libcamera::DmaBufMap::default())
    } else {
        None
    };

    // ctx_config は Send を実装しないため、await をまたがないようブロックで囲む
    let context = {
        #[allow(unused_mut)]
        let mut ctx_config = SoraConnectionContextConfig {
            adm_config,
            ..Default::default()
        };

        apply_video_toolbox_preference(
            &mut ctx_config.video_codec_preference,
            CodecDirection::Encoder,
            shiguredo_webrtc::VideoCodecType::H264,
            config.h264_encoder.as_deref(),
        )?;
        apply_video_toolbox_preference(
            &mut ctx_config.video_codec_preference,
            CodecDirection::Decoder,
            shiguredo_webrtc::VideoCodecType::H264,
            config.h264_decoder.as_deref(),
        )?;
        apply_video_toolbox_preference(
            &mut ctx_config.video_codec_preference,
            CodecDirection::Encoder,
            shiguredo_webrtc::VideoCodecType::H265,
            config.h265_encoder.as_deref(),
        )?;
        apply_video_toolbox_preference(
            &mut ctx_config.video_codec_preference,
            CodecDirection::Decoder,
            shiguredo_webrtc::VideoCodecType::H265,
            config.h265_decoder.as_deref(),
        )?;

        // NVIDIA NvCodec: capability 登録 + preference 設定
        #[cfg(feature = "nvcodec")]
        if let Ok(nvcodec) = NvCodecVideoCodecCapability::new() {
            ctx_config
                .video_codec_preference
                .merge(&VideoCodecPreference::new_from_capability(&nvcodec));
            ctx_config.video_codec_capabilities.push(Box::new(nvcodec));

            for (codec_type, encoder, decoder) in [
                (
                    shiguredo_webrtc::VideoCodecType::H264,
                    config.h264_encoder.as_deref(),
                    config.h264_decoder.as_deref(),
                ),
                (
                    shiguredo_webrtc::VideoCodecType::H265,
                    config.h265_encoder.as_deref(),
                    config.h265_decoder.as_deref(),
                ),
                (
                    shiguredo_webrtc::VideoCodecType::Av1,
                    config.av1_encoder.as_deref(),
                    config.av1_decoder.as_deref(),
                ),
            ] {
                apply_codec_preference(
                    &mut ctx_config.video_codec_preference,
                    CodecDirection::Encoder,
                    codec_type,
                    encoder,
                    "nvidia",
                    "nvidia",
                    "NVIDIA NVENC/NVDEC",
                )?;
                apply_codec_preference(
                    &mut ctx_config.video_codec_preference,
                    CodecDirection::Decoder,
                    codec_type,
                    decoder,
                    "nvidia",
                    "nvidia",
                    "NVIDIA NVENC/NVDEC",
                )?;
            }
        }

        // Intel oneVPL: capability 登録 + preference 設定
        #[cfg(feature = "vpl")]
        if let Ok(vpl) = VplVideoCodecCapability::new() {
            ctx_config
                .video_codec_preference
                .merge(&VideoCodecPreference::new_from_capability(&vpl));
            ctx_config.video_codec_capabilities.push(Box::new(vpl));

            for (codec_type, encoder, decoder) in [
                (
                    shiguredo_webrtc::VideoCodecType::Vp9,
                    config.vp9_encoder.as_deref(),
                    config.vp9_decoder.as_deref(),
                ),
                (
                    shiguredo_webrtc::VideoCodecType::H264,
                    config.h264_encoder.as_deref(),
                    config.h264_decoder.as_deref(),
                ),
                (
                    shiguredo_webrtc::VideoCodecType::H265,
                    config.h265_encoder.as_deref(),
                    config.h265_decoder.as_deref(),
                ),
                (
                    shiguredo_webrtc::VideoCodecType::Av1,
                    config.av1_encoder.as_deref(),
                    config.av1_decoder.as_deref(),
                ),
            ] {
                apply_codec_preference(
                    &mut ctx_config.video_codec_preference,
                    CodecDirection::Encoder,
                    codec_type,
                    encoder,
                    "vpl",
                    "vpl",
                    "Intel oneVPL",
                )?;
                apply_codec_preference(
                    &mut ctx_config.video_codec_preference,
                    CodecDirection::Decoder,
                    codec_type,
                    decoder,
                    "vpl",
                    "vpl",
                    "Intel oneVPL",
                )?;
            }
        }

        // AMD AMF: capability 登録 + preference 設定
        #[cfg(feature = "amf")]
        if let Ok(amf) = AmfVideoCodecCapability::new() {
            ctx_config
                .video_codec_preference
                .merge(&VideoCodecPreference::new_from_capability(&amf));
            ctx_config.video_codec_capabilities.push(Box::new(amf));

            for (codec_type, encoder, decoder) in [
                (
                    shiguredo_webrtc::VideoCodecType::H264,
                    config.h264_encoder.as_deref(),
                    config.h264_decoder.as_deref(),
                ),
                (
                    shiguredo_webrtc::VideoCodecType::H265,
                    config.h265_encoder.as_deref(),
                    config.h265_decoder.as_deref(),
                ),
                (
                    shiguredo_webrtc::VideoCodecType::Av1,
                    config.av1_encoder.as_deref(),
                    config.av1_decoder.as_deref(),
                ),
            ] {
                apply_codec_preference(
                    &mut ctx_config.video_codec_preference,
                    CodecDirection::Encoder,
                    codec_type,
                    encoder,
                    "amf",
                    "amf",
                    "AMD AMF",
                )?;
                apply_codec_preference(
                    &mut ctx_config.video_codec_preference,
                    CodecDirection::Decoder,
                    codec_type,
                    decoder,
                    "amf",
                    "amf",
                    "AMD AMF",
                )?;
            }
        }

        // V4L2 ハードウェアエンコーダーの追加
        if config.use_v4l2_encoder {
            #[cfg(feature = "raspberrypi")]
            {
                use crate::v4l2_encoder::sora_capability::V4l2VideoCodecCapability;
                info!(target: "sora", "using V4L2 H.264 hardware encoder capability");
                ctx_config
                    .video_codec_capabilities
                    .push(Box::new(V4l2VideoCodecCapability {
                        dmabuf_map: dmabuf_map.clone(),
                    }));
            }
            #[cfg(not(feature = "raspberrypi"))]
            {
                tracing::warn!(target: "sora", "--use-v4l2-encoder requires raspberrypi feature");
            }
        }
        // OpenH264 エンコーダー/デコーダーの追加
        if let Some(ref lib) = config.openh264_lib {
            use crate::openh264::sora_capability::Openh264VideoCodecCapability;
            info!(target: "sora", "using OpenH264 codec capability");
            ctx_config
                .video_codec_capabilities
                .push(Box::new(Openh264VideoCodecCapability { lib: lib.clone() }));
        }

        SoraConnectionContext::new_with_config(ctx_config).map_err(|e| -> BoxError {
            format!("SoraConnectionContext の生成に失敗: {e}").into()
        })?
    };

    // ── 映像キャプチャ・トラック ──────────────────────────────────────────
    #[cfg(feature = "raspberrypi")]
    let mut _libcamera_capture = None;

    let (video_track, _video_capture) = if config.role.wants_send()
        && config.video
        && !config.no_video_input_device
    {
        let source = AdaptedVideoTrackSource::new();
        let vts = source.cast_to_video_track_source();

        if config.use_libcamera {
            #[cfg(feature = "raspberrypi")]
            {
                let capture = crate::libcamera::start_libcamera_capture(
                    source,
                    config.video_width as u32,
                    config.video_height as u32,
                    config.libcamera_controls.clone(),
                    dmabuf_map.clone(),
                )?;
                _libcamera_capture = Some(capture);
                info!(target: "sora", width = config.video_width, height = config.video_height, "libcamera video started");
                let track = context.create_video_track(&vts).map_err(|e| -> BoxError {
                    format!("映像トラックの生成に失敗: {e}").into()
                })?;
                (Some(track), None)
            }
            #[cfg(not(feature = "raspberrypi"))]
            {
                let _ = (source, vts);
                return Err(
                    "--use-libcamera は raspberrypi feature が有効な場合のみ使用できます".into(),
                );
            }
        } else if config.fake_capture_device {
            start_fake_video_thread(
                source,
                config.video_width,
                config.video_height,
                config.framerate,
                beep_trigger.clone(),
            );
            info!(target: "sora", width = config.video_width, height = config.video_height, fps = config.framerate, "fake video started");
            let track = context.create_video_track(&vts).map_err(|e| -> BoxError {
                format!("映像トラックの生成に失敗: {e}").into()
            })?;
            (Some(track), None)
        } else {
            let shared = Arc::new(std::sync::Mutex::new((source, TimestampAligner::new())));

            let video_cfg = VideoCaptureConfig {
                device_id: config.video_input_device.clone(),
                width: config.video_width,
                height: config.video_height,
                fps: config.framerate as i32,
                pixel_format: config.force_pixel_format,
            };
            let mut capture = VideoCapture::new(video_cfg, move |frame| {
                let Some(buffer) = crate::webrtc_video::capture_frame_to_i420(
                    frame.pixel_format,
                    frame.data,
                    frame.stride,
                    frame.stride_uv,
                    frame.uv_data,
                    frame.width,
                    frame.height,
                ) else {
                    return;
                };
                let Ok(mut guard) = shared.lock() else { return };
                let (ref mut source, ref mut aligner) = *guard;

                let AdaptFrameResult { applied, size } =
                    source.adapt_frame(frame.width, frame.height, frame.timestamp_us);
                if !applied {
                    return;
                }

                let ts =
                    aligner.translate(frame.timestamp_us, shiguredo_webrtc::time_millis() * 1000);
                if size.adapted_width != frame.width || size.adapted_height != frame.height {
                    let mut scaled = I420Buffer::new(size.adapted_width, size.adapted_height);
                    scaled.scale_from(&buffer);

                    let video_frame = crate::webrtc_video::video_frame_from_i420(
                        &scaled,
                        ts,
                        (ts * 90 / 1000) as u32,
                    );
                    source.on_frame(&video_frame);
                } else {
                    let video_frame = crate::webrtc_video::video_frame_from_i420(
                        &buffer,
                        ts,
                        (ts * 90 / 1000) as u32,
                    );
                    source.on_frame(&video_frame);
                }
            })
            .map_err(|e| format!("映像キャプチャの初期化に失敗: {e}"))?;

            capture
                .start()
                .map_err(|e| format!("映像キャプチャの開始に失敗: {e}"))?;
            info!(target: "sora", "video capture started");

            let track = context.create_video_track(&vts).map_err(|e| -> BoxError {
                format!("映像トラックの生成に失敗: {e}").into()
            })?;
            (Some(track), Some(capture))
        }
    } else {
        if config.no_video_input_device {
            info!(target: "sora", "映像を無効化しました (--no-video-input-device)");
        }
        (None, None)
    };

    // ── 音声トラック ─────────────────────────────────────────────────────
    let audio_track = if config.role.wants_send() && config.audio {
        let audio_source = context.create_audio_source().map_err(|e| -> BoxError {
            format!("音声ソースの生成に失敗: {e}").into()
        })?;
        let track = context
            .create_audio_track(&audio_source)
            .map_err(|e| -> BoxError {
                format!("音声トラックの生成に失敗: {e}").into()
            })?;
        Some(track)
    } else {
        None
    };

    // ── SoraConnection ビルド ────────────────────────────────────────────
    // 受信トラックにアタッチする VideoSink はイベントハンドラが保持するレジストリで
    // 生存管理する。ハンドラは builder 経由で client に移動するため、
    // client の破棄と同時にレジストリも解放される。
    let handler = SoraEventHandler::new(
        Arc::clone(connected),
        #[cfg(feature = "player")]
        config.display_cmd_tx.clone(),
    );

    let mut builder = SoraConnection::builder(
        context,
        config.signaling_urls.clone(),
        config.channel_id.clone(),
        config.role,
        handler,
    );

    // 検証と変換は prepare ですでに済んでいる
    builder = builder
        .video(prepared.video.clone())
        .audio(prepared.audio.clone());

    if let Some(track) = video_track {
        builder = builder.sender_video_track(track);
    }
    if let Some(track) = audio_track {
        builder = builder.sender_audio_track(track);
    }
    if let Some(dc) = config.data_channel_signaling {
        builder = builder.data_channel_signaling(dc);
    }
    if let Some(idw) = config.ignore_disconnect_websocket {
        builder = builder.ignore_disconnect_websocket(idw);
    }
    if config.simulcast {
        builder = builder.simulcast(true);
    }
    if config.spotlight {
        builder = builder.spotlight(true);
    }
    if let Some(ref metadata) = prepared.metadata {
        builder = builder.metadata(metadata.clone());
    }
    // クライアント ID / バンドル ID は prepare で検証済みの値をそのまま渡す
    if let Some(client_id) = config.client_id.as_deref() {
        builder = builder.client_id(client_id.to_string());
    }
    if let Some(bundle_id) = config.bundle_id.as_deref() {
        builder = builder.bundle_id(bundle_id.to_string());
    }
    if let Some(ref metadata) = prepared.signaling_notify_metadata {
        builder = builder.signaling_notify_metadata(metadata.clone());
    }
    if !prepared.data_channels.is_empty() {
        info!(
            target: "sora",
            labels = ?config.data_channel_labels,
            "using user defined data channels"
        );
        builder = builder.data_channels(prepared.data_channels.clone());
    }
    if !prepared.forwarding_filters.is_empty() {
        info!(
            target: "sora",
            count = prepared.forwarding_filters.len(),
            "using forwarding filters"
        );
        builder = builder.forwarding_filters(prepared.forwarding_filters.clone());
    }
    if config.disconnect_wait_timeout > 0 {
        builder = builder.disconnect_wait_timeout(std::time::Duration::from_secs(
            config.disconnect_wait_timeout as u64,
        ));
    }
    if config.insecure {
        builder = builder.insecure(true).turn_tls_insecure(true);
    }
    // 接続ごとに設定を再適用するため、文字列は複製して渡す
    if let Some(ca_pem) = config.ca_cert.as_deref() {
        builder = builder.ca_cert(ca_pem.to_string());
    }
    if let Some((cert_pem, key_pem)) = config.client_cert.as_ref() {
        builder = builder.client_cert(cert_pem.clone(), key_pem.clone());
    }
    if let Some(proxy_url) = config.proxy_url.as_deref() {
        builder = builder.proxy(ProxyInfo {
            url: proxy_url.to_string(),
            username: config.proxy_username.clone(),
            password: config.proxy_password.clone(),
            user_agent: None,
        });
    }

    let (client, handle) = builder
        .build()
        .map_err(|e| -> BoxError { format!("SoraConnection のビルドに失敗: {e}").into() })?;

    // ── メトリクス stats プロバイダー登録 ─────────────────────────────────
    if let Some(ms) = metrics_state {
        let (stats_tx, mut stats_rx) = mpsc::channel::<oneshot::Sender<String>>(1);
        ms.register(stats_tx).await;

        let stats_handle = handle.clone();
        tokio::spawn(async move {
            while let Some(reply_tx) = stats_rx.recv().await {
                match stats_handle.get_stats().await {
                    Ok(json) => {
                        let _ = reply_tx.send(json.to_string());
                    }
                    Err(_) => {
                        let _ = reply_tx.send("[]".to_string());
                    }
                }
            }
        });
    }

    info!(
        target: "sora",
        signaling_urls = ?config.signaling_urls,
        channel_id = %config.channel_id,
        "connecting to Sora"
    );

    // ── 実行 ─────────────────────────────────────────────────────────────
    let result = client
        .run()
        .await
        .map_err(|e| -> BoxError { format!("Sora client error: {e}").into() });

    // キャプチャリソースの Drop 順序を明示
    drop(_video_capture);
    #[cfg(feature = "raspberrypi")]
    drop(_libcamera_capture);
    drop(audio_capture);
    drop(adm_state);

    result
}

// ─── VideoCodecPreference の設定 ──────────────────────────────────────────────

fn apply_video_toolbox_preference(
    preference: &mut VideoCodecPreference,
    direction: CodecDirection,
    codec_type: shiguredo_webrtc::VideoCodecType,
    value: Option<&str>,
) -> Result<(), BoxError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value != "videotoolbox" {
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        // Apple プラットフォーム以外では利用不可
        let _ = (preference, direction, codec_type);
        Err("videotoolbox is only available on Apple platforms".into())
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let Some(codec) = preference.find_mut(direction, codec_type) else {
            return Err(format!(
                "video codec preference not found: direction={direction:?}, codec_type={codec_type:?}"
            )
            .into());
        };
        codec.set_implementation(sora_sdk::VideoCodecImplementation::new(
            "internal-hwa",
            "WebRTC ObjC default VideoCodecFactory",
        ));
        Ok(())
    }
}

/// 指定されたコーデック方向・種別に対して、CLI で指定された実装名が
/// `target` と一致した場合に VideoCodecPreference を設定する汎用ヘルパー
#[expect(dead_code)]
fn apply_codec_preference(
    preference: &mut VideoCodecPreference,
    direction: CodecDirection,
    codec_type: shiguredo_webrtc::VideoCodecType,
    value: Option<&str>,
    target: &str,
    implementation_name: &'static str,
    implementation_description: &'static str,
) -> Result<(), BoxError> {
    let Some(v) = value else {
        return Ok(());
    };
    if v != target {
        return Ok(());
    }
    let Some(codec) = preference.find_mut(direction, codec_type) else {
        return Err(format!(
            "video codec preference not found: direction={direction:?}, codec_type={codec_type:?}"
        )
        .into());
    };
    codec.set_implementation(VideoCodecImplementation::new(
        implementation_name,
        implementation_description,
    ));
    Ok(())
}

// ─── ADM 構築 ─────────────────────────────────────────────────────────────────

/// 接続で使う音声デバイス
///
/// 接続が終了するまで生存させる必要がある。フェイク音声の場合は生成スレッドごと
/// 保持する。
struct AudioDevices {
    /// SoraConnectionContext に渡す AudioDeviceModule の設定
    adm: AdmConfig,
    /// 音声再生・収録の状態共有
    state: Option<AdmState>,
    /// 実デバイスの音声キャプチャ
    capture: Option<AudioCapture>,
    /// フェイク音声の生成者 (--fake-capture-device)
    fake: Option<crate::fake::FakeAudioCapturer>,
}

/// 音声デバイスを構築する
///
/// 再接続のたびに呼び直される。実デバイスを使う場合はその都度開き直す。
/// sora_sdk が `AdmConfig` 経由で AudioDeviceModule の所有を取するため、
/// 接続をまたいだ共用はできない。
fn build_audio_devices(
    config: &SoraConfig,
    beep_trigger: Option<&crate::fake::BeepTrigger>,
) -> Result<AudioDevices, BoxError> {
    if config.no_audio_device {
        info!(target: "sora", "audio device disabled (--no-audio-device)");
        return Ok(AudioDevices {
            adm: AdmConfig::NoAudioDevice,
            state: None,
            capture: None,
            fake: None,
        });
    }

    // フェイク音声は実デバイスを開かず、FakeAudioCapturer の AudioDeviceModule を使う
    if let Some(trigger) = beep_trigger {
        info!(target: "sora", "fake audio started (--fake-capture-device)");
        let mut capturer = crate::fake::FakeAudioCapturer::new(trigger.clone());
        capturer.start();
        return Ok(AudioDevices {
            adm: AdmConfig::UseExternal(capturer.audio_device_module()),
            state: None,
            capture: None,
            fake: Some(capturer),
        });
    }

    let state = AdmState::new();
    state.set_audio_output_device(config.audio_output_device.clone());
    let handler = build_adm_handler(&state);
    let adm = AudioDeviceModule::new_with_handler(handler);

    let state_for_cap = state.clone();
    let audio_cfg = AudioCaptureConfig {
        device_id: crate::audio_device::resolve_audio_device_id(
            config.audio_input_device.as_deref(),
            shiguredo_audio_device::AudioDeviceType::Input,
        )
        .map_err(|e| format!("音声入力デバイスの指定に失敗: {e}"))?,
        ..Default::default()
    };
    let mut capture = AudioCapture::new(audio_cfg, move |frame| {
        state_for_cap.on_audio_frame(&frame);
    })
    .map_err(|e| format!("音声キャプチャの初期化に失敗: {e}"))?;

    capture
        .start()
        .map_err(|e| format!("音声キャプチャの開始に失敗: {e}"))?;
    info!(target: "sora", "audio capture started");

    Ok(AudioDevices {
        adm: AdmConfig::UseExternal(adm),
        state: Some(state),
        capture: Some(capture),
        fake: None,
    })
}

/// JSON 文字列を sora_sdk の検証済み JSON 型へ変換する
fn parse_json_string(text: &str) -> Result<JsonString, BoxError> {
    text.parse()
        .map_err(|e| -> BoxError { format!("JSON のパースに失敗: {e}").into() })
}

// ─── Video/Audio ヘルパー ─────────────────────────────────────────────────────

fn build_video(enabled: bool, codec_type: Option<&str>, bit_rate: u32) -> Result<Video, BoxError> {
    if !enabled {
        return Ok(Video::new_bool(false));
    }
    let br = if bit_rate > 0 { Some(bit_rate) } else { None };
    match codec_type {
        None => Ok(Video::new_bool(true)),
        Some("VP8") => Ok(Video::new_vp8(br)),
        Some("VP9") => Ok(Video::new_vp9(br, None)),
        Some("AV1") => Ok(Video::new_av1(br, None)),
        Some("H264") => Ok(Video::new_h264(br, None)),
        Some("H265") => Ok(Video::new_h265(br, None)),
        Some(other) => Err(format!(
            "不正な video-codec-type: {other} (VP8, VP9, AV1, H264, H265 のいずれかを指定してください)"
        )
        .into()),
    }
}

fn build_audio(enabled: bool, codec_type: Option<&str>, bit_rate: u32) -> Result<Audio, BoxError> {
    if !enabled {
        return Ok(Audio::new_bool(false));
    }
    let br = if bit_rate > 0 { Some(bit_rate) } else { None };
    match codec_type {
        None => Ok(Audio::new_bool(true)),
        Some("OPUS") => Ok(Audio::new_opus(br, None)),
        Some(other) => {
            Err(format!("不正な audio-codec-type: {other} (OPUS のみ対応しています)").into())
        }
    }
}
