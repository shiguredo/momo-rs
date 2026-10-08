//! Ayame モードの実装
//!
//! ayame-rust-sdk の接続ロジックを 1 ファイルに統合した実装。
//! 映像は raden でアニメーションフレームを生成するフェイクソースを使用する。

mod message;
mod signaling;
mod webrtc;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nojson::Json;
use shiguredo_webrtc::IceCandidate;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::metrics::MetricsState;

use self::message::{PongMessage, ReceivedMessage, RegisterMessage};
use self::signaling::{SignalingCommand, SignalingNotification, start_signaling_task};
use self::webrtc::{
    AyameEngine, add_transceivers, create_and_send_offer, create_peer, handle_answer, handle_offer,
};
use crate::error::BoxError;

// ─── 設定 ─────────────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct AyameConfig {
    pub signaling_url: String,
    pub room_id: String,
    pub client_id: Option<String>,
    pub signaling_key: Option<String>,
    pub direction: Direction,
    pub no_google_stun: bool,
    pub no_audio_device: bool,
    pub no_video_input_device: bool,
    pub fake_capture_device: bool,
    pub use_libcamera: bool,
    pub use_libcamera_native: bool,
    pub libcamera_controls: Vec<(String, String)>,
    pub use_v4l2_encoder: bool,
    pub openh264_lib: Option<shiguredo_openh264::Openh264Library>,
    /// `--{codec}-encoder` / `--{codec}-decoder` によるコーデック選択
    pub codec_selection: crate::video_codec_factory::CodecSelection,
    pub video_input_device: Option<String>,
    pub audio_input_device: Option<String>,
    /// `--audio-output-device` で指定されたデバイス (インデックス番号・デバイス名・unique_id)
    pub audio_output_device: Option<String>,
    pub video_width: i32,
    pub video_height: i32,
    pub framerate: u32,
    pub insecure: bool,
    pub force_pixel_format: Option<shiguredo_video_device::PixelFormat>,
    /// クライアント証明書 (cert_pem, key_pem)
    pub client_cert: Option<(String, String)>,
    /// CA 証明書 (PEM)
    pub ca_cert: Option<String>,
    pub degradation_preference: shiguredo_webrtc::DegradationPreference,
    pub video_codec_type: Option<String>,
    pub audio_codec_type: Option<String>,
    #[cfg(target_os = "linux")]
    pub serial: Option<crate::serial::SerialConfig>,
}

pub enum Direction {
    SendRecv,
    SendOnly,
    RecvOnly,
}

// ─── 定数 ─────────────────────────────────────────────────────────────────────

/// 接続開始時の監視期限秒数 (momo の kInitialWatchdogTimeoutSeconds)
const INITIAL_WATCHDOG_SECONDS: u64 = 30;
/// 接続確立後の監視期限秒数 (momo の kConnectedWatchdogTimeoutSeconds)
const CONNECTED_WATCHDOG_SECONDS: u64 = 60;
/// 再接続間隔の刻み秒数 (momo の kReconnectIntervalStepSeconds)
const RECONNECT_INTERVAL_STEP_SECONDS: u64 = 10;
/// 再接続間隔の上限秒数 (momo の kReconnectIntervalMaxSeconds)
const RECONNECT_INTERVAL_MAX_SECONDS: u64 = 30;

// ─── 公開 API ─────────────────────────────────────────────────────────────────

/// 再接続間隔の下限秒数
///
/// momo は WebSocket が閉じた瞬間から再接続待ちに入る。工場と実デバイスは作り直さないが、
/// 確立とクローズが速いケースでループが濃密にならないよう 1 秒は空ける。
const MIN_RECONNECT_SECONDS: u64 = 1;

/// Ayame サーバーへの接続を繰り返す
///
/// 接続が終了すれば再接続を続ける。momo (C++) と同じく、WebSocket を確立したうえでの
/// 終了は直ちに、確立できなかった場合は回数に応じて待ってから再試行する。
pub async fn run(
    config: AyameConfig,
    metrics_state: Option<Arc<MetricsState>>,
) -> Result<(), BoxError> {
    // クライアント ID (未指定時はランダム 8 バイトを hex 文字列に)
    let client_id = config.client_id.clone().unwrap_or_else(|| {
        let mut bytes = [0u8; 8];
        aws_lc_rs::rand::fill(&mut bytes).expect("BUG: 乱数生成に失敗しました");
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    });

    // WebRTC エンジンは再接続をまたいで使い回す。momo も PeerConnectionFactory と
    // キャプチャは作り直さず、PeerConnection と WebSocket だけを接続ごとに作り直す
    let engine = AyameEngine::new(&config)?;

    let mut retry: u32 = 0;

    loop {
        match connect_once(&config, &client_id, &engine, metrics_state.as_ref()).await {
            // WebSocket を確立できていれば再接続回数を数え直す (momo の OnClose 相当)
            Attempt::ReconnectNow => {
                retry = 0;
                info!(target: "ayame", "reconnecting immediately");
            }
            Attempt::ReconnectLater(error) => {
                warn!(target: "ayame", error = %error, "connection failed");
            }
        }

        let wait_seconds = (u64::from(retry) * RECONNECT_INTERVAL_STEP_SECONDS)
            .clamp(MIN_RECONNECT_SECONDS, RECONNECT_INTERVAL_MAX_SECONDS);
        retry = retry.saturating_add(1);
        info!(
            target: "ayame",
            retry_seconds = wait_seconds,
            "reconnecting to Ayame"
        );
        tokio::time::sleep(std::time::Duration::from_secs(wait_seconds)).await;
    }
}

/// 1 回の接続の実行結果
enum Attempt {
    /// WebSocket を確立したうえで接続が終わった。即再接続する
    ReconnectNow,
    /// WebSocket を確立できなかった。待ってから再試行する
    ReconnectLater(BoxError),
}

/// 1 回の Ayame 接続を実行する
///
/// 返り値のいかんにかかわらず `run` が再接続する。
async fn connect_once(
    config: &AyameConfig,
    client_id: &str,
    engine: &AyameEngine,
    metrics_state: Option<&Arc<MetricsState>>,
) -> Attempt {
    let watchdog = Watchdog::new();

    // シグナリングタスク起動
    let (cmd_tx, mut notify_rx) = match start_signaling_task(
        &config.signaling_url,
        config.insecure,
        config.client_cert.clone(),
        config.ca_cert.clone(),
    ) {
        Ok(channels) => channels,
        Err(error) => return Attempt::ReconnectLater(error),
    };

    // watchdog: 期限を過ぎた接続を閉じて、再接続経路に乗せる
    // 接続が終わったら送信側をドロップしてタスクを終了させる
    let (_watchdog_keepalive, mut stop_rx) = mpsc::channel::<()>(1);
    {
        let watchdog = watchdog.clone();
        let cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop_rx.recv() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                }
                if watchdog.expired() {
                    warn!(target: "ayame", "watchdog expired, closing signaling connection");
                    let _ = cmd_tx.send(SignalingCommand::Close).await;
                    return;
                }
            }
        });
    }

    // WebSocket 接続完了を待つ
    match notify_rx.recv().await {
        Some(SignalingNotification::Connected) => {}
        Some(SignalingNotification::Error(e)) => return Attempt::ReconnectLater(e),
        Some(SignalingNotification::Closed) => {
            return Attempt::ReconnectLater("ハンドシェイク前に接続が閉じました".into());
        }
        _ => {
            return Attempt::ReconnectLater("予期しない通知を受信しました".into());
        }
    }
    info!(target: "ayame", "WebSocket 接続完了");
    // ここから先は WebSocket を確立している
    let established = || Attempt::ReconnectNow;

    // register 送信
    let register = RegisterMessage {
        room_id: &config.room_id,
        client_id,
        signaling_key: config.signaling_key.as_deref(),
    };
    if cmd_tx
        .send(SignalingCommand::SendText(Json(&register).to_string()))
        .await
        .is_err()
    {
        return established();
    }
    info!(target: "ayame", room_id = %config.room_id, client_id = %client_id, "register 送信");

    // accept/reject 待ち
    let accept = loop {
        match notify_rx.recv().await {
            Some(SignalingNotification::Message(ReceivedMessage::Accept(accept))) => {
                break accept;
            }
            Some(SignalingNotification::Message(ReceivedMessage::Reject { reason })) => {
                // momo は reject を特別扱いしないが、無限の即再接続を避けて
                // 接続失敗と同じ扱いにして間隔を伸ばす
                return Attempt::ReconnectLater(format!("reject: {reason}").into());
            }
            Some(SignalingNotification::Error(e)) => return Attempt::ReconnectLater(e),
            Some(SignalingNotification::Closed) | None => {
                return established();
            }
            _ => continue,
        }
    };
    info!(target: "ayame", is_exist_user = accept.is_exist_user, "accept 受信");

    // PeerConnection 作成
    let peer = match create_peer(
        engine,
        cmd_tx.clone(),
        &accept,
        config.no_google_stun,
        watchdog.clone(),
        #[cfg(target_os = "linux")]
        config.serial.clone(),
    ) {
        Ok(peer) => peer,
        Err(error) => {
            warn!(target: "ayame", error = %error, "failed to create peer connection");
            return established();
        }
    };

    // メトリクス stats チャンネル
    let (stats_tx, mut stats_rx) = mpsc::channel::<tokio::sync::oneshot::Sender<String>>(1);
    if let Some(ms) = metrics_state {
        ms.register(stats_tx).await;
    }

    // オファー側 / アンサー側 で分岐
    // WebRTC の SDP 操作は C++ スレッドからのコールバックを同期的に待つため block_in_place を使用
    if accept.is_exist_user {
        let offer_result = tokio::task::block_in_place(|| {
            add_transceivers(
                &peer.pc,
                &engine.factory,
                &config.direction,
                engine.video_track_source.as_ref(),
                config.degradation_preference,
                config.video_codec_type.as_deref(),
                config.audio_codec_type.as_deref(),
            )?;
            create_and_send_offer(&peer.pc, &cmd_tx)
        });
        if let Err(error) = offer_result {
            warn!(target: "ayame", error = %error, "failed to create offer");
            return established();
        }
    }

    // メッセージループ
    loop {
        tokio::select! {
            notification = notify_rx.recv() => {
                match notification {
                    Some(SignalingNotification::Message(msg)) => match msg {
                        ReceivedMessage::Offer { sdp } => {
                            // トラックソースは参照カウントなので再接続をまたいで複製する
                            let vs = engine.video_track_source.clone();
                            let result = tokio::task::block_in_place(|| {
                                handle_offer(
                                    &peer.pc,
                                    &sdp,
                                    &cmd_tx,
                                    &engine.factory,
                                    &config.direction,
                                    vs,
                                    config.degradation_preference,
                                )
                            });
                            if let Err(error) = result {
                                warn!(target: "ayame", error = %error, "failed to handle offer");
                                return established();
                            }
                        }
                        ReceivedMessage::Answer { sdp } => {
                            let result =
                                tokio::task::block_in_place(|| handle_answer(&peer.pc, &sdp));
                            if let Err(error) = result {
                                warn!(target: "ayame", error = %error, "failed to handle answer");
                                return established();
                            }
                        }
                        ReceivedMessage::Candidate {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                        } => {
                            if let Ok(ice) = IceCandidate::new(&sdp_mid, sdp_mline_index, &candidate) {
                                let _ = peer.pc.add_ice_candidate(&ice);
                            }
                        }
                        ReceivedMessage::Ping => {
                            // momo と同じく ping 受信だけで監視期限を延ばす
                            watchdog.arm();
                            let _ = cmd_tx
                                .send(SignalingCommand::SendText(Json(&PongMessage).to_string()))
                                .await;
                            info!(target: "ayame", "ping → pong");
                        }
                        ReceivedMessage::Bye => {
                            // momo は bye でも終了せず再接続に入る
                            info!(target: "ayame", "bye 受信、再接続します");
                            break;
                        }
                        _ => {}
                    },
                    Some(SignalingNotification::Error(e)) => {
                        warn!(target: "ayame", error = %e, "シグナリングエラー");
                        break;
                    }
                    Some(SignalingNotification::Closed) | None => {
                        info!(target: "ayame", "シグナリング接続がクローズされました");
                        break;
                    }
                    Some(SignalingNotification::Connected) => {}
                }
            }

            Some(reply_tx) = stats_rx.recv() => {
                peer.pc.get_stats(move |report| {
                    let json = report.to_json().unwrap_or_else(|_| "[]".to_string());
                    let _ = reply_tx.send(json);
                });
            }
        }
    }

    let _ = cmd_tx.send(SignalingCommand::Close).await;
    established()
}

// ─── watchdog ────────────────────────────────────────────────────────────────

/// シグナリング接続の死活監視
///
/// momo の watchdog と同じ形にする。接続開始時は 30 秒、PeerConnection が connected に
/// なった後は 60 秒を期限とし、シグナリングで `ping` を受信したときだけ期限を延ばす。
/// 期限を過ぎたら接続を閉じ、再接続経路に乗せる。
#[derive(Clone)]
pub(super) struct Watchdog {
    /// 現在の期限秒数
    interval_seconds: Arc<AtomicU64>,
    /// 期限を迎える時刻 (unix ミリ秒)
    deadline_millis: Arc<AtomicU64>,
}

impl Watchdog {
    /// 監視を開始する (momo の Watchdog::Enable 相当)
    fn new() -> Self {
        let watchdog = Self {
            interval_seconds: Arc::new(AtomicU64::new(INITIAL_WATCHDOG_SECONDS)),
            deadline_millis: Arc::new(AtomicU64::new(0)),
        };
        watchdog.arm();
        watchdog
    }

    /// 現在の期限で監視を再接続する (momo の Watchdog::Reset 相当)
    fn arm(&self) {
        let interval = self.interval_seconds.load(Ordering::Relaxed);
        self.deadline_millis
            .store(unix_millis() + interval * 1000, Ordering::Relaxed);
    }

    /// PeerConnection の接続確立後に期限を 60 秒へ切り替える
    pub(super) fn on_connected(&self) {
        self.interval_seconds
            .store(CONNECTED_WATCHDOG_SECONDS, Ordering::Relaxed);
        self.arm();
    }

    /// 期限を過ぎたかどうか
    fn expired(&self) -> bool {
        unix_millis() >= self.deadline_millis.load(Ordering::Relaxed)
    }
}

/// unix エポックからの経過ミリ秒
fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 監視開始時は momo と同じ 30 秒の期限でアームされていること
    #[test]
    fn watchdog_starts_with_initial_deadline() {
        let watchdog = Watchdog::new();
        let remaining = watchdog
            .deadline_millis
            .load(Ordering::Relaxed)
            .saturating_sub(unix_millis());

        assert!(
            (INITIAL_WATCHDOG_SECONDS * 1000 - 2_000..=INITIAL_WATCHDOG_SECONDS * 1000)
                .contains(&remaining),
            "開始時の残り期限は約 30 秒のはず: {remaining} ミリ秒"
        );
        assert!(!watchdog.expired(), "開始直後は期限前のはず");
    }

    /// 接続確立後は期限が 60 秒へ切り替わり、誤再接続しないこと
    #[test]
    fn watchdog_switches_to_connected_deadline() {
        let watchdog = Watchdog::new();
        watchdog.on_connected();

        assert_eq!(
            watchdog.interval_seconds.load(Ordering::Relaxed),
            CONNECTED_WATCHDOG_SECONDS,
            "接続確立後は期限が 60 秒に切り替わるはず"
        );
        assert!(
            watchdog.deadline_millis.load(Ordering::Relaxed) > unix_millis() + 50_000,
            "接続直後の残りは 50 秒を超えているはず (実測で ayame-labo の ping は約 5 秒間隔)"
        );
        assert!(!watchdog.expired(), "接続直後は期限前のはず");
    }

    /// 期限を過ぎれば検知され、延長 (momo の Reset 相当) で回復すること
    #[test]
    fn watchdog_detects_silence_and_recovers_on_arm() {
        let watchdog = Watchdog::new();

        watchdog
            .deadline_millis
            .store(unix_millis() - 1, Ordering::Relaxed);
        assert!(watchdog.expired(), "期限を過ぎれば再接続対象になるはず");

        watchdog.arm();
        assert!(!watchdog.expired(), "arm 後は現在の期限で受け直すはず");

        // 接続確立後に arm すれば 60 秒で受け直される
        watchdog.on_connected();
        watchdog.arm();
        assert!(
            watchdog.deadline_millis.load(Ordering::Relaxed) > unix_millis() + 50_000,
            "接続確立後の arm は 60 秒で受け直されるはず"
        );
    }
}
