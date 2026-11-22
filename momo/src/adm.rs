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
use shiguredo_webrtc::{AudioDeviceModuleHandler, AudioParameters, AudioTransportPtr};
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
    // 音声デバイスの同期コールバックと WebRTC の登録・解除を直列化する。
    // チャネルで別スレッドに委ねず、ポインターの参照中は解除側を待たせる。
    audio_transport: Arc<Mutex<Option<AudioTransportPtr>>>,
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
    fn on_recorded_data(&self, samples: &[i16], n_channels: usize, samples_per_sec: u32) {
        if !self.recording.load(Ordering::SeqCst) {
            return;
        }
        let transport_guard = self
            .audio_transport
            .lock()
            .expect(AUDIO_TRANSPORT_LOCK_POISONED);
        let Some(transport) = transport_guard.as_ref() else {
            return;
        };
        let mut new_mic_level = 0u32;
        // 安全性: 登録解除は同じロックを取得するため、呼び出しが終わるまで
        // C++ 側の transport は有効。入力は i16 に整列した PCM で、チャンネルごとの
        // フレーム数はスライス長から求める。出力先は書き込み可能なローカル変数。
        let _ = unsafe {
            transport.recorded_data_is_available(
                samples.as_ptr().cast(),
                samples.len() / n_channels,
                size_of::<i16>() * n_channels,
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
        // WebRTC の AudioTransport はモノラルとステレオを受け付ける。
        if !(1..=2).contains(&frame.channels) {
            return;
        }
        let n_channels = frame.channels as usize;
        let Ok(samples_per_sec) = u32::try_from(frame.sample_rate) else {
            return;
        };
        match frame.format {
            AudioFormat::S16 => {
                if let Some(samples) = frame.as_s16() {
                    self.on_recorded_data(samples, n_channels, samples_per_sec);
                }
            }
            AudioFormat::F32 => {
                if let Some(samples) = frame.as_f32() {
                    let converted: Vec<i16> = samples
                        .iter()
                        .map(|&sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                    self.on_recorded_data(&converted, n_channels, samples_per_sec);
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

impl AudioDeviceModuleHandler for ExternalAdmHandler {
    fn register_audio_callback(&mut self, audio_transport: Option<AudioTransportPtr>) -> i32 {
        // WebRTC は transport を破棄する前に None を渡す。進行中の呼び出しを
        // 待ってからポインターを外し、その後のコールバックには触らせない。
        *self
            .state
            .audio_transport
            .lock()
            .expect(AUDIO_TRANSPORT_LOCK_POISONED) = audio_transport;
        0
    }

    fn init(&mut self) -> i32 {
        0
    }

    fn terminate(&mut self) -> i32 {
        self.register_audio_callback(None)
    }

    fn initialized(&mut self) -> bool {
        true
    }

    // ── 録音方向 ──

    fn recording_devices(&mut self) -> i16 {
        1
    }

    fn recording_device_name(&mut self, index: u16) -> Option<(String, String)> {
        if index == 0 {
            Some((
                "External Recording".to_string(),
                "external-recording".to_string(),
            ))
        } else {
            None
        }
    }

    fn recording_is_available(&mut self, available: &mut bool) -> i32 {
        *available = true;
        0
    }

    fn init_recording(&mut self) -> i32 {
        0
    }

    fn recording_is_initialized(&mut self) -> bool {
        true
    }

    fn start_recording(&mut self) -> i32 {
        self.state.recording.store(true, Ordering::SeqCst);
        0
    }

    fn stop_recording(&mut self) -> i32 {
        self.state.recording.store(false, Ordering::SeqCst);
        0
    }

    fn recording(&mut self) -> bool {
        self.state.recording.load(Ordering::SeqCst)
    }

    // ── 再生方向 ──

    fn playout_devices(&mut self) -> i16 {
        let devices = self.state.playout_device_list();
        devices.len() as i16
    }

    fn playout_device_name(&mut self, index: u16) -> Option<(String, String)> {
        let devices = self.state.playout_device_list();
        devices.get(index as usize).cloned()
    }

    fn playout_is_available(&mut self, available: &mut bool) -> i32 {
        *available = true;
        0
    }

    fn init_playout(&mut self) -> i32 {
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

    fn playout_is_initialized(&mut self) -> bool {
        self.state
            .audio_playback
            .lock()
            .expect(AUDIO_PLAYBACK_LOCK_POISONED)
            .is_some()
    }

    fn start_playout(&mut self) -> i32 {
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

    fn stop_playout(&mut self) -> i32 {
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

    fn playing(&mut self) -> bool {
        self.state.playout.load(Ordering::SeqCst)
    }

    fn stereo_playout_is_available(&mut self, available: &mut bool) -> i32 {
        *available = true;
        0
    }

    fn set_stereo_playout(&mut self, _enable: bool) -> i32 {
        0
    }

    fn stereo_playout(&mut self, enabled: &mut bool) -> i32 {
        *enabled = true;
        0
    }

    fn playout_delay(&mut self, delay_ms: &mut u16) -> i32 {
        *delay_ms = 0;
        0
    }

    fn get_playout_underrun_count(&mut self) -> i32 {
        0
    }

    fn get_playout_audio_parameters(&mut self, params: &mut Option<AudioParameters>) -> i32 {
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
        if !(1..=2).contains(&channels) {
            return None;
        }
        let frames_usize = usize::try_from(frames).ok()?;
        let channels_usize = channels as usize;
        let samples_per_sec = u32::try_from(sample_rate).ok()?;
        let sample_count = frames_usize.checked_mul(channels_usize)?;

        // WebRTC は S16 のポインターとして書き込むため、i16 のアラインメントを確保する。
        let mut audio_samples = vec![0i16; sample_count];
        let mut n_samples_out = 0usize;
        let mut elapsed_time_ms = 0i64;
        let mut ntp_time_ms = 0i64;

        let result = {
            let transport_guard = audio_transport.lock().expect(AUDIO_TRANSPORT_LOCK_POISONED);
            let transport = transport_guard.as_ref()?;
            // 安全性: 呼び出し中は登録解除と同じロックを保持する。バッファーは
            // フレーム数 × チャンネル数の i16 として書き込み可能で、他の出力先も
            // 書き込み可能なローカル変数を指す。バイト数は全チャンネルの 1 フレーム分。
            unsafe {
                transport.need_more_play_data(
                    frames_usize,
                    size_of::<i16>() * channels_usize,
                    channels_usize,
                    samples_per_sec,
                    audio_samples.as_mut_ptr().cast(),
                    &mut n_samples_out,
                    &mut elapsed_time_ms,
                    &mut ntp_time_ms,
                )
            }
        };
        if result != 0 {
            warn!(target: "adm", result, "playout need_more_play_data failed");
            return None;
        }

        // WebRTC が返す n_samples_out は全チャンネル分のサンプル数。完全なフレームだけを返す。
        let frames_out = n_samples_out.min(sample_count) / channels_usize;
        PlaybackFrame::from_s16(
            &audio_samples[..frames_out * channels_usize],
            channels,
            sample_rate,
        )
        .ok()
    };

    let config = AudioPlaybackConfig {
        device_id,
        sample_rate: 48000,
        channels: 2,
    };

    AudioPlayback::new(config, callback).map_err(|e| format!("音声出力デバイスの初期化に失敗: {e}"))
}
