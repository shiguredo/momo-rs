//! AudioDeviceModule 状態管理
//!
//! p2p / ayame / sora の各モードで共通の ADM 状態と AudioDeviceModuleHandler 構築ロジック。
//!
//! 録音方向は AudioCapture のコールバック → [`AdmState::on_audio_frame`] →
//! `recorded_data_is_available` という経路でマイクの音声を WebRTC に渡す。
//! 再生方向は WebRTC が `start_playout` を呼んだときに AudioPlayback を開始し、
//! デバイスのコールバックで `need_more_play_data` を呼んで受信音声を取り出す。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use shiguredo_audio_device::{
    AudioDeviceList, AudioFormat, AudioFrame, AudioPlayback, AudioPlaybackConfig, PlaybackFrame,
};
use shiguredo_webrtc::{AudioDeviceModuleHandler, AudioParameters, AudioTransportRef};
use tracing::{info, warn};

use crate::audio_device::resolve_audio_device_in_list;

const AUDIO_TRANSPORT_LOCK_POISONED: &str = "audio_transport lock poisoned";
const AUDIO_PLAYBACK_LOCK_POISONED: &str = "audio_playback lock poisoned";
const PLAYOUT_DEVICE_LIST_LOCK_POISONED: &str = "playout_device_list lock poisoned";
const PLAYOUT_DEVICE_SPEC_LOCK_POISONED: &str = "playout_device_spec lock poisoned";

// ─── ADM 状態 ─────────────────────────────────────────────────────────────────

/// AudioDeviceModule の録音・再生状態を保持する共有状態
#[derive(Clone)]
pub struct AdmState {
    recording: Arc<AtomicBool>,
    audio_transport: Arc<Mutex<Option<AudioTransportRef>>>,
    playout: Arc<AtomicBool>,
    audio_playback: Arc<Mutex<Option<AudioPlayback>>>,
    playout_device_spec: Arc<Mutex<Option<String>>>,
    playout_device_list: Arc<Mutex<Vec<(String, String)>>>,
}

impl AdmState {
    pub fn new() -> Self {
        Self {
            recording: Arc::new(AtomicBool::new(false)),
            audio_transport: Arc::new(Mutex::new(None)),
            playout: Arc::new(AtomicBool::new(false)),
            audio_playback: Arc::new(Mutex::new(None)),
            playout_device_spec: Arc::new(Mutex::new(None)),
            playout_device_list: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// `--audio-output-device` の指定文字列を設定する
    ///
    /// 値は `--list-devices` の表示順のインデックス番号、デバイス名、または
    /// unique_id のいずれかを指定できる。None の場合はデフォルトデバイスを使用する。
    pub fn set_audio_output_device(&self, spec: Option<String>) {
        *self
            .playout_device_spec
            .lock()
            .expect(PLAYOUT_DEVICE_SPEC_LOCK_POISONED) = spec;
    }

    /// 録音データを WebRTC の AudioTransport に転送する
    pub fn on_recorded_data(
        &self,
        audio_data: *const u8,
        n_samples: usize,
        n_bytes_per_sample: usize,
        n_channels: usize,
        samples_per_sec: u32,
    ) {
        if !self.recording.load(Ordering::SeqCst) {
            return;
        }
        let transport = {
            *self
                .audio_transport
                .lock()
                .expect(AUDIO_TRANSPORT_LOCK_POISONED)
        };
        let Some(transport) = transport else { return };
        let mut new_mic_level = 0u32;
        let _ = unsafe {
            transport.recorded_data_is_available(
                audio_data,
                n_samples,
                n_bytes_per_sample,
                n_channels,
                samples_per_sec,
                0,
                0,
                0,
                false,
                &mut new_mic_level,
                None,
            )
        };
    }

    /// AudioCapture コールバックから受け取ったフレームを WebRTC に転送する
    ///
    /// S16 はそのまま転送し、F32 は S16 に変換してから転送する。
    pub fn on_audio_frame(&self, frame: &AudioFrame<'_>) {
        let n_channels = frame.channels as usize;
        let samples_per_sec = frame.sample_rate as u32;
        match frame.format {
            AudioFormat::S16 => {
                self.on_recorded_data(
                    frame.data.as_ptr(),
                    frame.frames as usize,
                    2 * n_channels,
                    n_channels,
                    samples_per_sec,
                );
            }
            AudioFormat::F32 => {
                if let Some(f32_data) = frame.as_f32() {
                    let s16: Vec<i16> = f32_data
                        .iter()
                        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                    self.on_recorded_data(
                        s16.as_ptr() as *const u8,
                        frame.frames as usize,
                        2 * n_channels,
                        n_channels,
                        samples_per_sec,
                    );
                }
            }
        }
    }

    /// 再生デバイス一覧を取得する
    ///
    /// 列挙はデバイスサーバー (CoreAudio / PulseAudio) へのアクセスを伴い
    /// コストがかかるため、結果はキャッシュする。
    /// 列挙に失敗した場合は空一覧と警告ログを返す。
    fn playout_device_list(&self) -> Vec<(String, String)> {
        {
            let guard = self
                .playout_device_list
                .lock()
                .expect(PLAYOUT_DEVICE_LIST_LOCK_POISONED);
            if !guard.is_empty() {
                return guard.clone();
            }
        }

        let devices: Vec<(String, String)> = match AudioDeviceList::enumerate_output() {
            Ok(list) => list
                .as_slice()
                .iter()
                .filter_map(|d| Some((d.name().ok()?, d.unique_id().ok()?)))
                .collect(),
            Err(e) => {
                warn!(target: "adm", error = %e, "playout device enumeration failed");
                Vec::new()
            }
        };

        let mut guard = self
            .playout_device_list
            .lock()
            .expect(PLAYOUT_DEVICE_LIST_LOCK_POISONED);
        *guard = devices.clone();
        devices
    }
}

// ─── AudioDeviceModuleHandler 構築 ──────────────────────────────────────────

/// 外部音声キャプチャ・再生用の AudioDeviceModuleHandler 実装
struct ExternalAdmHandler {
    state: AdmState,
}

// SAFETY: AudioTransportRef は WebRTC 内部で排他制御されているため
// ExternalAdmHandler をスレッド間で共有しても安全である。
unsafe impl Sync for ExternalAdmHandler {}

impl AudioDeviceModuleHandler for ExternalAdmHandler {
    fn register_audio_callback(&self, audio_transport: Option<AudioTransportRef>) -> i32 {
        *self
            .state
            .audio_transport
            .lock()
            .expect(AUDIO_TRANSPORT_LOCK_POISONED) = audio_transport;
        0
    }

    fn init(&self) -> i32 {
        0
    }

    fn terminate(&self) -> i32 {
        0
    }

    fn initialized(&self) -> bool {
        true
    }

    // ── 録音方向 ──

    fn recording_devices(&self) -> i16 {
        1
    }

    fn recording_device_name(&self, index: u16) -> Option<(String, String)> {
        if index == 0 {
            Some((
                "External Recording".to_string(),
                "external-recording".to_string(),
            ))
        } else {
            None
        }
    }

    fn recording_is_available(&self, available: &mut bool) -> i32 {
        *available = true;
        0
    }

    fn init_recording(&self) -> i32 {
        0
    }

    fn recording_is_initialized(&self) -> bool {
        true
    }

    fn start_recording(&self) -> i32 {
        self.state.recording.store(true, Ordering::SeqCst);
        0
    }

    fn stop_recording(&self) -> i32 {
        self.state.recording.store(false, Ordering::SeqCst);
        0
    }

    fn recording(&self) -> bool {
        self.state.recording.load(Ordering::SeqCst)
    }

    // ── 再生方向 ──

    fn playout_devices(&self) -> i16 {
        let devices = self.state.playout_device_list();
        devices.len() as i16
    }

    fn playout_device_name(&self, index: u16) -> Option<(String, String)> {
        let devices = self.state.playout_device_list();
        devices.get(index as usize).cloned()
    }

    fn playout_is_available(&self, available: &mut bool) -> i32 {
        *available = true;
        0
    }

    fn init_playout(&self) -> i32 {
        let state = &self.state;

        let device_list = state.playout_device_list();
        let spec = state
            .playout_device_spec
            .lock()
            .expect(PLAYOUT_DEVICE_SPEC_LOCK_POISONED)
            .clone();

        let device_id = match spec.as_deref() {
            None => None,
            Some(spec) => match resolve_audio_device_in_list(spec, &device_list) {
                Ok(unique_id) => {
                    info!(target: "adm", spec, unique_id = %unique_id, "playout device resolved");
                    Some(unique_id)
                }
                Err(e) => {
                    // 指定したデバイスが見つからない場合はデフォルトデバイスで再生する
                    warn!(target: "adm", error = %e, "playout device not found; falling back to default output device");
                    None
                }
            },
        };

        match create_audio_playback(state, device_id) {
            Ok(playback) => {
                let mut guard = state
                    .audio_playback
                    .lock()
                    .expect(AUDIO_PLAYBACK_LOCK_POISONED);
                *guard = Some(playback);
                info!(target: "adm", "playout initialized");
                0
            }
            Err(e) => {
                // プレイアウトが初期化できない場合でも録画・映像は動作させたいため、
                // エラーを返して WebRTC 側に playout を無効にさせる
                warn!(target: "adm", error = %e, "playout initialization failed");
                -1
            }
        }
    }

    fn playout_is_initialized(&self) -> bool {
        self.state
            .audio_playback
            .lock()
            .expect(AUDIO_PLAYBACK_LOCK_POISONED)
            .is_some()
    }

    fn start_playout(&self) -> i32 {
        let state = &self.state;
        {
            let mut guard = state
                .audio_playback
                .lock()
                .expect(AUDIO_PLAYBACK_LOCK_POISONED);
            let Some(playback) = guard.as_mut() else {
                warn!(target: "adm", "playout not initialized");
                return -1;
            };
            if let Err(e) = playback.start() {
                warn!(target: "adm", error = %e, "playout start failed");
                return -1;
            }
        }
        state.playout.store(true, Ordering::SeqCst);
        info!(target: "adm", "playout started");
        0
    }

    fn stop_playout(&self) -> i32 {
        let state = &self.state;
        state.playout.store(false, Ordering::SeqCst);
        if let Some(playback) = state
            .audio_playback
            .lock()
            .expect(AUDIO_PLAYBACK_LOCK_POISONED)
            .as_mut()
        {
            playback.stop();
        }
        info!(target: "adm", "playout stopped");
        0
    }

    fn playing(&self) -> bool {
        self.state.playout.load(Ordering::SeqCst)
    }

    fn stereo_playout_is_available(&self, available: &mut bool) -> i32 {
        *available = true;
        0
    }

    fn set_stereo_playout(&self, _enable: bool) -> i32 {
        0
    }

    fn stereo_playout(&self, enabled: &mut bool) -> i32 {
        *enabled = true;
        0
    }

    fn playout_delay(&self, delay_ms: &mut u16) -> i32 {
        *delay_ms = 0;
        0
    }

    fn get_playout_underrun_count(&self) -> i32 {
        0
    }

    fn get_playout_audio_parameters(&self, params: &mut Option<AudioParameters>) -> i32 {
        // 再生フォーマットは 48kHz / ステレオ / 10ms フレーム固定
        // webrtc の収録フォーマットと一致しており、デバイス側がリサンプリングする
        *params = Some(AudioParameters::new(48000, 2, 480));
        0
    }
}

pub fn build_adm_handler(state: &AdmState) -> Box<dyn AudioDeviceModuleHandler> {
    Box::new(ExternalAdmHandler {
        state: state.clone(),
    })
}

/// AudioPlayback を生成する
///
/// コールバックは AudioPlayback のデバイスコールバックから呼ばれ、
/// WebRTC の AudioTransport から再生データ (S16) を取得して返す。
///
/// プレイアウト中でない場合や AudioTransport が未登録の場合は無音を返す。
fn create_audio_playback(
    state: &AdmState,
    device_id: Option<String>,
) -> Result<AudioPlayback, String> {
    // AudioPlayback のコールバックは AdmState の全フィールドを参照すると
    // 循環参照 (state -> audio_playback -> コールバック -> state) になるため、
    // 必要な Arc だけをキャプチャする
    let playout = Arc::clone(&state.playout);
    let audio_transport = Arc::clone(&state.audio_transport);

    let callback = move |frames: i32, channels: i32, sample_rate: i32| -> Option<PlaybackFrame> {
        if !playout.load(Ordering::SeqCst) {
            return None;
        }
        let transport_guard = audio_transport.lock().expect(AUDIO_TRANSPORT_LOCK_POISONED);
        let transport = transport_guard.as_ref()?;

        let frames_usize = frames as usize;
        let channels_usize = channels as usize;
        let byte_len = match frames_usize
            .checked_mul(channels_usize)
            .and_then(|n| n.checked_mul(2))
        {
            Some(n) => n,
            None => {
                warn!(target: "adm", frames, channels, "playout invalid buffer size");
                return None;
            }
        };

        // S16 固定でデータを受け取る。書き込まれなかった分はゼロ (無音) で埋まる
        let mut audio_samples = vec![0u8; byte_len];
        let mut n_samples_out = 0usize;
        let mut elapsed_time_ms = 0i64;
        let mut ntp_time_ms = 0i64;

        // SAFETY: audio_samples は byte_len 分の書き込み可能な領域を指しており、
        // n_samples_out は書き込み可能なポインタである
        let result = unsafe {
            transport.need_more_play_data(
                frames_usize,
                2,
                channels_usize,
                sample_rate as u32,
                audio_samples.as_mut_ptr(),
                &mut n_samples_out,
                &mut elapsed_time_ms,
                &mut ntp_time_ms,
            )
        };
        if result != 0 {
            warn!(target: "adm", result, "playout need_more_play_data failed");
            return None;
        }

        // n_samples_out はサンプルごとのフレーム数を表す
        let frames_out = n_samples_out.min(frames_usize);
        if frames_out < frames_usize {
            audio_samples.truncate(frames_out * channels_usize * 2);
        }

        Some(PlaybackFrame {
            data: audio_samples,
            frames: frames_out as i32,
            channels,
            sample_rate,
            format: AudioFormat::S16,
        })
    };

    let config = AudioPlaybackConfig {
        device_id,
        sample_rate: 48000,
        channels: 2,
    };

    AudioPlayback::new(config, callback).map_err(|e| format!("音声出力デバイスの初期化に失敗: {e}"))
}
