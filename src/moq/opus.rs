//! Opus のエンコード・デコードと OpusHead の取り扱い
//!
//! 音声は Opus を使い、Audio Config (0x0F) に OpusHead (RFC 7845 §5.1) を載せる。
//! LOC は draft-ietf-moq-loc-04、MSF は draft-ietf-moq-msf-01 に基づく。draft 由来のため
//! 将来の改版で変わる可能性がある。

use shiguredo_opus::{Application, Decoder, DecoderConfig, Encoder, EncoderConfig, FrameDuration};

use crate::moq::error::MoqError;

/// 音声のサンプリングレート (Hz)
pub const SAMPLE_RATE: u32 = 48_000;
/// 音声のチャンネル数 (ステレオ)
pub const CHANNELS: u8 = 2;
/// Opus のフレーム長 (ミリ秒)
///
/// レイテンシとフレームサイズのトレードオフから 20 ms 固定にする。
const FRAME_DURATION_MS: u32 = 20;

/// OpusHead (RFC 7845 §5.1) を組み立てる
///
/// Pre-skip と Output Gain は 0、Channel Mapping Family は 0 (mono / stereo) とする。
pub fn build_opus_head(sample_rate: u32, channels: u8) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // Version
    head.push(channels); // Channel Count
    head.extend_from_slice(&0u16.to_le_bytes()); // Pre-skip
    head.extend_from_slice(&sample_rate.to_le_bytes()); // Input Sample Rate
    head.extend_from_slice(&0u16.to_le_bytes()); // Output Gain
    head.push(0); // Channel Mapping Family
    head
}

/// パース済みの OpusHead
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusHead {
    /// チャンネル数
    pub channel_count: u8,
    /// 入力サンプリングレート。0 は「未指定」を意味する
    pub input_sample_rate: u32,
}

/// OpusHead (RFC 7845 §5.1) をパースする
pub fn parse_opus_head(bytes: &[u8]) -> Result<OpusHead, MoqError> {
    if bytes.len() < 19 || &bytes[0..8] != b"OpusHead" {
        return Err(MoqError::Media("invalid OpusHead magic".to_owned()));
    }
    if bytes[8] != 1 {
        return Err(MoqError::Media(format!(
            "unsupported OpusHead version: {}",
            bytes[8]
        )));
    }
    let channel_count = bytes[9];
    if channel_count == 0 {
        return Err(MoqError::Media("OpusHead channel count is 0".to_owned()));
    }
    let input_sample_rate = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    Ok(OpusHead {
        channel_count,
        input_sample_rate,
    })
}

/// Opus エンコーダ
pub struct OpusEncoder {
    encoder: Encoder,
}

impl OpusEncoder {
    /// Opus エンコーダを作る
    ///
    /// `bitrate` は bits per second。
    pub fn new(sample_rate: u32, channels: u8, bitrate: u32) -> Result<Self, MoqError> {
        let mut config = EncoderConfig::new(sample_rate, channels);
        config.bitrate = Some(bitrate);
        config.application = Some(Application::Audio);
        config.frame_duration = Some(FrameDuration::Ms20);
        let encoder = Encoder::new(config)
            .map_err(|e| MoqError::Media(format!("failed to create Opus encoder: {e}")))?;
        tracing::info!(
            target: "moq",
            sample_rate = sample_rate,
            channels = channels,
            bitrate = bitrate,
            frame_duration_ms = FRAME_DURATION_MS,
            "Opus encoder created"
        );
        Ok(Self { encoder })
    }

    /// 1 フレームをエンコードする
    ///
    /// `pcm` はインターリーブ済みの i16 で、`samples_per_frame() * channels` サンプル必要。
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, MoqError> {
        self.encoder
            .encode(pcm)
            .map_err(|e| MoqError::Media(format!("failed to encode Opus frame: {e}")))
    }

    /// 1 フレームあたりのサンプル数 (チャンネル単位)
    pub fn samples_per_frame(&self) -> usize {
        self.encoder.frame_samples()
    }
}

/// Opus デコーダ
pub struct OpusDecoder {
    decoder: Decoder,
    sample_rate: u32,
    channels: u8,
}

impl OpusDecoder {
    /// Opus デコーダを作る
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self, MoqError> {
        let decoder = Decoder::new(DecoderConfig::new(sample_rate, channels))
            .map_err(|e| MoqError::Media(format!("failed to create Opus decoder: {e}")))?;
        Ok(Self {
            decoder,
            sample_rate,
            channels,
        })
    }

    /// 1 パケットをデコードして PCM (S16 インターリーブ) を返す
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>, MoqError> {
        self.decoder
            .decode(data)
            .map_err(|e| MoqError::Media(format!("failed to decode Opus packet: {e}")))
    }

    /// サンプリングレート
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// チャンネル数
    pub fn channels(&self) -> u8 {
        self.channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpusHead が 19 バイトで組み立てられること
    #[test]
    fn test_build_opus_head_format() {
        let head = build_opus_head(SAMPLE_RATE, CHANNELS);
        assert_eq!(head.len(), 19);
        assert_eq!(&head[0..8], b"OpusHead");
        assert_eq!(head[8], 1);
        assert_eq!(head[9], CHANNELS);
        assert_eq!(&head[10..12], &[0x00, 0x00]);
        assert_eq!(&head[12..16], &SAMPLE_RATE.to_le_bytes());
        assert_eq!(&head[16..18], &[0x00, 0x00]);
        assert_eq!(head[18], 0);
    }

    /// 組み立てた OpusHead をパースできること
    #[test]
    fn test_parse_opus_head_round_trip() {
        let head = build_opus_head(SAMPLE_RATE, CHANNELS);
        let parsed = parse_opus_head(&head).expect("パースに成功すること");
        assert_eq!(parsed.channel_count, CHANNELS);
        assert_eq!(parsed.input_sample_rate, SAMPLE_RATE);
    }

    /// 短いデータを拒否すること
    #[test]
    fn test_parse_opus_head_rejects_short_data() {
        assert!(parse_opus_head(&[0u8; 18]).is_err());
    }

    /// Magic が違うデータを拒否すること
    #[test]
    fn test_parse_opus_head_rejects_invalid_magic() {
        let mut head = build_opus_head(SAMPLE_RATE, CHANNELS);
        head[0] = b'X';
        assert!(parse_opus_head(&head).is_err());
    }

    /// チャンネル数 0 を拒否すること
    #[test]
    fn test_parse_opus_head_rejects_zero_channels() {
        let mut head = build_opus_head(SAMPLE_RATE, CHANNELS);
        head[9] = 0;
        assert!(parse_opus_head(&head).is_err());
    }
}
