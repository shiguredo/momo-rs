//! MOQT モードのメディアキャプチャと変換
//!
//! sora-moq の publisher が使う映像・音声のキャプチャと、エンコーダーへ渡す形式への
//! 変換をまとめる。疑似キャプチャ (`--fake-capture-device`) もここで扱う。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use shiguredo_audio_device::{AudioCapture, AudioCaptureConfig, AudioDeviceType, AudioFormat};
use shiguredo_video_device::{
    PixelFormat, VideoCapture, VideoCaptureConfig, VideoFrame as DeviceVideoFrame,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::error::BoxError;
use crate::moq::config::MoqCommonConfig;
use crate::moq::error::MoqError;

/// キャプチャした I420 フレーム
///
/// 各プレーンは 1 バイト 1 サンプルで詰めてある (ストライドなし)。
#[derive(Debug, Clone)]
pub struct CapturedVideoFrame {
    /// Y プレーン
    pub y: Vec<u8>,
    /// U プレーン
    pub u: Vec<u8>,
    /// V プレーン
    pub v: Vec<u8>,
    /// 幅 (px)
    pub width: usize,
    /// 高さ (px)
    pub height: usize,
    /// キャプチャ時刻 (マイクロ秒)
    pub timestamp_us: i64,
}

/// キャプチャした音声フレーム
#[derive(Debug, Clone)]
pub struct CapturedAudioFrame {
    /// S16 インターリーブの PCM
    pub pcm: Vec<i16>,
    /// チャンネル数
    pub channels: u8,
    /// サンプリングレート (Hz)
    pub sample_rate: u32,
    /// キャプチャ時刻 (マイクロ秒)
    pub timestamp_us: i64,
}

/// 映像キャプチャの保持ガード
///
/// drop するとキャプチャを停止する。publisher が生存する間は保持すること。
pub struct VideoCaptureGuard {
    /// 実デバイスのキャプチャ
    capture: Option<VideoCapture>,
    /// 疑似キャプチャのスレッド
    fake_thread: Option<std::thread::JoinHandle<()>>,
    /// 疑似キャプチャの停止フラグ
    fake_stop: Arc<AtomicBool>,
}

impl Drop for VideoCaptureGuard {
    fn drop(&mut self) {
        self.fake_stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.fake_thread.take() {
            let _ = thread.join();
        }
        if let Some(mut capture) = self.capture.take() {
            capture.stop();
        }
    }
}

/// 音声キャプチャの保持ガード
pub struct AudioCaptureGuard {
    /// 実デバイスのキャプチャ
    capture: Option<AudioCapture>,
    /// 疑似キャプチャのスレッド
    fake_thread: Option<std::thread::JoinHandle<()>>,
    /// 疑似キャプチャの停止フラグ
    fake_stop: Arc<AtomicBool>,
}

impl Drop for AudioCaptureGuard {
    fn drop(&mut self) {
        self.fake_stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.fake_thread.take() {
            let _ = thread.join();
        }
        if let Some(mut capture) = self.capture.take() {
            capture.stop();
        }
    }
}

/// 映像キャプチャを開始する
pub fn start_video_capture(
    common: &MoqCommonConfig,
    sender: mpsc::Sender<CapturedVideoFrame>,
) -> Result<VideoCaptureGuard, BoxError> {
    if common.fake_capture_device {
        info!(target: "moq", "using fake video capture");
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            let width = common.video_width as usize;
            let height = common.video_height as usize;
            let fps = common.framerate;
            std::thread::Builder::new()
                .name("moq-fake-video".to_owned())
                .spawn(move || run_fake_video(width, height, fps, sender, stop))
                .map_err(|e| MoqError::Media(format!("failed to spawn fake video thread: {e}")))?
        };
        return Ok(VideoCaptureGuard {
            capture: None,
            fake_thread: Some(thread),
            fake_stop: stop,
        });
    }

    let config = VideoCaptureConfig {
        device_id: common.video_input_device.clone(),
        width: common.video_width,
        height: common.video_height,
        fps: common.framerate as i32,
        pixel_format: None,
    };
    let mut capture = VideoCapture::new(config, move |frame| {
        if let Some(converted) = convert_device_frame(&frame) {
            // 容量オーバーはフレーム破棄 (リアルタイム性を優先する)
            let _ = sender.try_send(converted);
        }
    })
    .map_err(|e| MoqError::Media(format!("failed to create video capture: {e}")))?;
    capture
        .start()
        .map_err(|e| MoqError::Media(format!("failed to start video capture: {e}")))?;
    info!(target: "moq", "video capture started");
    Ok(VideoCaptureGuard {
        capture: Some(capture),
        fake_thread: None,
        fake_stop: Arc::new(AtomicBool::new(false)),
    })
}

/// 音声キャプチャを開始する
pub fn start_audio_capture(
    common: &MoqCommonConfig,
    sender: mpsc::Sender<CapturedAudioFrame>,
) -> Result<AudioCaptureGuard, BoxError> {
    if common.fake_capture_device {
        info!(target: "moq", "using fake audio capture");
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("moq-fake-audio".to_owned())
                .spawn(move || run_fake_audio(sender, stop))
                .map_err(|e| MoqError::Media(format!("failed to spawn fake audio thread: {e}")))?
        };
        return Ok(AudioCaptureGuard {
            capture: None,
            fake_thread: Some(thread),
            fake_stop: stop,
        });
    }

    let device_id = crate::audio_device::resolve_audio_device_id(
        common.audio_input_device.as_deref(),
        AudioDeviceType::Input,
    )
    .map_err(|e| MoqError::Media(format!("failed to resolve audio input device: {e}")))?;
    let config = AudioCaptureConfig {
        device_id,
        sample_rate: crate::moq::opus::SAMPLE_RATE as i32,
        channels: crate::moq::opus::CHANNELS as i32,
    };
    let mut capture = AudioCapture::new(config, move |frame| {
        let pcm = match frame.format {
            AudioFormat::S16 => frame.as_s16().map(<[i16]>::to_vec).unwrap_or_default(),
            AudioFormat::F32 => frame
                .as_f32()
                .map(|samples| {
                    samples
                        .iter()
                        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect()
                })
                .unwrap_or_default(),
        };
        if pcm.is_empty() {
            return;
        }
        let converted = CapturedAudioFrame {
            pcm,
            channels: frame.channels as u8,
            sample_rate: frame.sample_rate as u32,
            timestamp_us: frame.timestamp_us,
        };
        let _ = sender.try_send(converted);
    })
    .map_err(|e| MoqError::Media(format!("failed to create audio capture: {e}")))?;
    capture
        .start()
        .map_err(|e| MoqError::Media(format!("failed to start audio capture: {e}")))?;
    info!(target: "moq", "audio capture started");
    Ok(AudioCaptureGuard {
        capture: Some(capture),
        fake_thread: None,
        fake_stop: Arc::new(AtomicBool::new(false)),
    })
}

/// 実デバイスのフレームを I420 のプレーンに変換する
///
/// 対応するピクセルフォーマットは I420 / NV12 / YUY2。それ以外は `None` を返す。
fn convert_device_frame(frame: &DeviceVideoFrame<'_>) -> Option<CapturedVideoFrame> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    if width == 0 || height == 0 {
        return None;
    }
    let stride = frame.stride.max(0) as usize;
    let stride_uv = frame.stride_uv.max(0) as usize;
    let (y, u, v) = match frame.pixel_format {
        PixelFormat::I420 => {
            let uv = frame.uv_data?;
            let chroma_height = height.div_ceil(2);
            if uv.len() < stride_uv * chroma_height * 2 {
                warn!(target: "moq", "I420 frame is shorter than expected");
                return None;
            }
            let mut y = vec![0u8; width * height];
            copy_plane(frame.data, stride, &mut y, width, width, height);
            let mut u = vec![0u8; width.div_ceil(2) * chroma_height];
            let mut v = vec![0u8; width.div_ceil(2) * chroma_height];
            copy_plane(
                &uv[..stride_uv * chroma_height],
                stride_uv,
                &mut u,
                width.div_ceil(2),
                width.div_ceil(2),
                chroma_height,
            );
            copy_plane(
                &uv[stride_uv * chroma_height..],
                stride_uv,
                &mut v,
                width.div_ceil(2),
                width.div_ceil(2),
                chroma_height,
            );
            (y, u, v)
        }
        PixelFormat::Nv12 => {
            let uv = frame.uv_data?;
            let chroma_height = height.div_ceil(2);
            if frame.data.len() < stride * height || uv.len() < stride_uv * chroma_height {
                warn!(target: "moq", "NV12 frame is shorter than expected");
                return None;
            }
            let mut y = vec![0u8; width * height];
            copy_plane(frame.data, stride, &mut y, width, width, height);
            let (u, v) = nv12_to_i420_planes(uv, stride_uv, width, height);
            (y, u, v)
        }
        PixelFormat::Yuy2 => {
            if frame.data.len() < stride * height {
                warn!(target: "moq", "YUY2 frame is shorter than expected");
                return None;
            }
            let (y, u, v) = yuy2_to_i420_planes(frame.data, stride, width, height);
            (y, u, v)
        }
        other => {
            warn!(target: "moq", format = %other, "unsupported pixel format for MOQT publishing");
            return None;
        }
    };
    Some(CapturedVideoFrame {
        y,
        u,
        v,
        width,
        height,
        timestamp_us: frame.timestamp_us,
    })
}

/// ストライド付きプレーンを詰めたプレーンにコピーする
fn copy_plane(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    row_bytes: usize,
    rows: usize,
) {
    for row in 0..rows {
        let src_start = row.saturating_mul(src_stride);
        let dst_start = row.saturating_mul(dst_stride);
        if src_start + row_bytes > src.len() || dst_start + row_bytes > dst.len() {
            return;
        }
        dst[dst_start..dst_start + row_bytes]
            .copy_from_slice(&src[src_start..src_start + row_bytes]);
    }
}

/// NV12 の UV プレーンを U / V プレーンに分離する (BT.601 色空間は変更しない)
fn nv12_to_i420_planes(
    uv: &[u8],
    stride_uv: usize,
    width: usize,
    height: usize,
) -> (Vec<u8>, Vec<u8>) {
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let mut u = vec![0u8; chroma_width * chroma_height];
    let mut v = vec![0u8; chroma_width * chroma_height];
    for row in 0..chroma_height {
        let src_start = row.saturating_mul(stride_uv);
        for col in 0..chroma_width {
            let src = src_start + col * 2;
            if src + 1 >= uv.len() {
                break;
            }
            let dst = row * chroma_width + col;
            u[dst] = uv[src];
            v[dst] = uv[src + 1];
        }
    }
    (u, v)
}

/// YUY2 パック形式を I420 のプレーンに変換する
///
/// YUY2 は `Y0 U Y1 V` の 4 バイトで 2 ピクセルを表す。I420 の色差は 2x2 ブロックで
/// 1 サンプルになるため、偶数行の色差をそのまま使う (垂直方向の平均は行わない)。
fn yuy2_to_i420_planes(
    data: &[u8],
    stride: usize,
    width: usize,
    height: usize,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let mut y = vec![0u8; width * height];
    let mut u = vec![0u8; chroma_width * chroma_height];
    let mut v = vec![0u8; chroma_width * chroma_height];
    for row in 0..height {
        let src_start = row.saturating_mul(stride);
        for col in 0..chroma_width {
            let src = src_start + col * 4;
            if src + 3 >= data.len() {
                break;
            }
            let dst_y = row * width + col * 2;
            if dst_y < y.len() {
                y[dst_y] = data[src];
                if dst_y + 1 < y.len() {
                    y[dst_y + 1] = data[src + 2];
                }
            }
            if row % 2 == 0 {
                let dst = (row / 2) * chroma_width + col;
                u[dst] = data[src + 1];
                v[dst] = data[src + 3];
            }
        }
    }
    (y, u, v)
}

/// BGRA (raden の Prgb32) を I420 のプレーンに変換する (BT.601 限定レンジ)
fn bgra_to_i420_planes(bgra: &[u8], width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let mut y = vec![0u8; width * height];
    let mut u = vec![0u8; chroma_width * chroma_height];
    let mut v = vec![0u8; chroma_width * chroma_height];
    for row in 0..height {
        for col in 0..width {
            let px = (row * width + col) * 4;
            if px + 2 >= bgra.len() {
                break;
            }
            let b = bgra[px] as i32;
            let g = bgra[px + 1] as i32;
            let r = bgra[px + 2] as i32;
            let luma = 16 + ((66 * r + 129 * g + 25 * b) >> 8);
            y[row * width + col] = luma.clamp(16, 235) as u8;
            if row % 2 == 0 && col % 2 == 0 {
                let chroma_u = 128 + ((-38 * r - 74 * g + 112 * b) >> 8);
                let chroma_v = 128 + ((112 * r - 94 * g - 18 * b) >> 8);
                let dst = (row / 2) * chroma_width + col / 2;
                u[dst] = chroma_u.clamp(16, 240) as u8;
                v[dst] = chroma_v.clamp(16, 240) as u8;
            }
        }
    }
    (y, u, v)
}

/// 疑似映像を生成し続けるスレッド
fn run_fake_video(
    width: usize,
    height: usize,
    fps: u32,
    sender: mpsc::Sender<CapturedVideoFrame>,
    stop: Arc<AtomicBool>,
) {
    let mut image = raden::Image::new(width as u32, height as u32, raden::PixelFormat::Prgb32);
    let mut runtime = raden::PipelineRuntime::new();
    let frame_duration = std::time::Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)));
    let start = std::time::Instant::now();
    let mut frame_index = 0u64;
    while !stop.load(Ordering::Relaxed) {
        {
            let mut ctx = raden::Context::new(&mut image, &mut runtime);
            let hue = (frame_index % 360) as f32;
            let (r, g, b) = hue_to_rgb(hue);
            ctx.set_fill_style(raden::Rgba32::rgb(r, g, b));
            ctx.fill_all();
            let max_x = (width as u64).saturating_sub(64);
            let rect_x = if max_x > 0 {
                (frame_index * 2 % max_x) as f64
            } else {
                0.0
            };
            ctx.set_fill_style(raden::Rgba32::rgb(255, 255, 255));
            ctx.fill_rect(&raden::Rect::new(rect_x, 10.0, 64.0, 64.0));
        }
        let (y, u, v) = bgra_to_i420_planes(image.data(), width, height);
        let frame = CapturedVideoFrame {
            y,
            u,
            v,
            width,
            height,
            timestamp_us: start.elapsed().as_micros() as i64,
        };
        if sender.try_send(frame).is_err() {
            // 受信側が詰まっている場合はフレームを落とす
        }
        frame_index += 1;
        std::thread::sleep(frame_duration);
    }
    info!(target: "moq", "fake video thread stopped");
}

/// 色相 (0.0..360.0) から RGB (各 0..255) に変換する
fn hue_to_rgb(hue: f32) -> (u8, u8, u8) {
    let h = hue / 60.0;
    let i = h as i32;
    let f = h - i as f32;
    let q = ((1.0 - f) * 255.0) as u8;
    let t = (f * 255.0) as u8;
    let v = 255u8;
    match i % 6 {
        0 => (v, t, 0),
        1 => (q, v, 0),
        2 => (0, v, t),
        3 => (0, q, v),
        4 => (t, 0, v),
        _ => (v, 0, q),
    }
}

/// 疑似音声 (440 Hz の正弦波) を生成し続けるスレッド
fn run_fake_audio(sender: mpsc::Sender<CapturedAudioFrame>, stop: Arc<AtomicBool>) {
    use std::f64::consts::PI;

    let sample_rate = crate::moq::opus::SAMPLE_RATE;
    let channels = crate::moq::opus::CHANNELS as usize;
    // 20 ms ぶんのフレームを作る
    let samples_per_frame = (sample_rate as usize / 1000) * 20;
    let frame_duration = std::time::Duration::from_millis(20);
    let start = std::time::Instant::now();
    let mut position = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let mut pcm = Vec::with_capacity(samples_per_frame * channels);
        for index in 0..samples_per_frame {
            let t = (position + index as u64) as f64 / sample_rate as f64;
            let value = (2.0 * PI * 440.0 * t).sin();
            let sample = (value * 0.2 * i16::MAX as f64) as i16;
            for _ in 0..channels {
                pcm.push(sample);
            }
        }
        position += samples_per_frame as u64;
        let frame = CapturedAudioFrame {
            pcm,
            channels: crate::moq::opus::CHANNELS,
            sample_rate,
            timestamp_us: start.elapsed().as_micros() as i64,
        };
        if sender.try_send(frame).is_err() {
            // 受信側が詰まっている場合はフレームを落とす
        }
        std::thread::sleep(frame_duration);
    }
    info!(target: "moq", "fake audio thread stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NV12 の UV プレーンを U / V に分離できること
    #[test]
    fn test_nv12_to_i420_planes() {
        // 4x2 の NV12: UV は 2x1
        let uv = [1u8, 2, 3, 4];
        let (u, v) = nv12_to_i420_planes(&uv, 4, 4, 2);
        assert_eq!(u, vec![1, 3]);
        assert_eq!(v, vec![2, 4]);
    }

    /// YUY2 を I420 に変換できること
    #[test]
    fn test_yuy2_to_i420_planes() {
        // 4x2 の YUY2: 1 行 8 バイトで 2 行
        let data = [
            10, 100, 11, 110, 12, 101, 13, 111, // 1 行目
            20, 102, 21, 112, 22, 103, 23, 113, // 2 行目
        ];
        let (y, u, v) = yuy2_to_i420_planes(&data, 8, 4, 2);
        assert_eq!(y, vec![10, 11, 12, 13, 20, 21, 22, 23]);
        assert_eq!(u, vec![100, 101]);
        assert_eq!(v, vec![110, 111]);
    }

    /// BGRA を I420 に変換できること
    #[test]
    fn test_bgra_to_i420_planes() {
        // 白 2x2
        let bgra = [255u8; 16];
        let (y, u, v) = bgra_to_i420_planes(&bgra, 2, 2);
        assert_eq!(y.len(), 4);
        assert_eq!(u.len(), 1);
        assert_eq!(v.len(), 1);
        // 白は BT.601 限定レンジで 235 付近になる
        assert!(y.iter().all(|value| *value > 200));
    }

    /// ストライド付きプレーンを詰めてコピーできること
    #[test]
    fn test_copy_plane() {
        let src = [1u8, 2, 99, 3, 4, 99];
        let mut dst = [0u8; 4];
        copy_plane(&src, 3, &mut dst, 2, 2, 2);
        assert_eq!(dst, [1, 2, 3, 4]);
    }

    /// 色相から RGB を生成できること
    #[test]
    fn test_hue_to_rgb() {
        assert_eq!(hue_to_rgb(0.0), (255, 0, 0));
        assert_eq!(hue_to_rgb(120.0), (0, 255, 0));
        assert_eq!(hue_to_rgb(240.0), (0, 0, 255));
    }
}
