//! MSF カタログの生成と解析
//!
//! draft-ietf-moq-msf-01 に基づく。draft 由来のため将来の改版で変わる可能性がある。
//! publisher は `catalog` トラックにカタログを 1 Object として publish し、
//! subscriber はカタログから映像と音声のトラックを見つける。

use shiguredo_moqt::msf::{
    MSF_CATALOG_TRACK_NAME, MsfCatalog, MsfCatalogDocument, MsfPackaging, MsfTrack,
};

use crate::moq::error::MoqError;

/// カタログを載せるトラック名
pub const CATALOG_TRACK_NAME: &[u8] = MSF_CATALOG_TRACK_NAME;

/// 映像トラックの仕様 (publisher 側)
#[derive(Debug, Clone)]
pub struct VideoTrackSpec {
    /// RFC 6381 形式の codec 文字列 (`avc1.PPCCLL`)
    pub codec: String,
    /// エンコード幅 (px)
    pub width: u64,
    /// エンコード高さ (px)
    pub height: u64,
    /// フレームレート (fps)
    pub framerate: f64,
    /// 最大ビットレート (kbps)
    pub bitrate_kbps: u32,
    /// LOC の Timescale
    pub timescale: u64,
    /// 最大 GOP 長 (ms)
    pub max_gop_duration_ms: u64,
}

/// 音声トラックの仕様 (publisher 側)
#[derive(Debug, Clone)]
pub struct AudioTrackSpec {
    /// RFC 6381 形式の codec 文字列 (`opus`)
    pub codec: String,
    /// サンプリングレート (Hz)
    pub samplerate: u64,
    /// チャンネル設定 (`"2"` はステレオ)
    pub channel_config: String,
    /// 最大ビットレート (kbps)
    pub bitrate_kbps: u32,
}

/// カタログ内で見つけた映像トラック
#[derive(Debug, Clone)]
pub struct VideoTrackInfo {
    /// トラック名
    pub track_name: Vec<u8>,
    /// codec 文字列
    pub codec: String,
    /// 幅 (px)
    pub width: u32,
    /// 高さ (px)
    pub height: u32,
    /// フレームレート (fps)
    pub framerate: u32,
}

/// カタログ内で見つけた音声トラック
#[derive(Debug, Clone)]
pub struct AudioTrackInfo {
    /// トラック名
    pub track_name: Vec<u8>,
    /// codec 文字列
    pub codec: String,
    /// サンプリングレート (Hz)
    pub samplerate: u32,
    /// チャンネル数
    pub channels: u8,
}

/// カタログを JSON にエンコードする
///
/// publish するトラックが 1 本も無い場合はエラーを返す。
pub fn encode_catalog(
    namespace: &str,
    video: Option<&VideoTrackSpec>,
    audio: Option<&AudioTrackSpec>,
) -> Result<Vec<u8>, MoqError> {
    let mut tracks = Vec::new();
    if let Some(video) = video {
        let mut track = MsfTrack::new("video".to_owned(), MsfPackaging::Loc, true);
        track.namespace = Some(namespace.to_owned());
        track.role = Some("video".to_owned());
        track.codec = Some(video.codec.clone());
        track.width = Some(video.width);
        track.height = Some(video.height);
        track.framerate = Some(video.framerate);
        track.bitrate = Some(video.bitrate_kbps as u64 * 1000);
        track.timescale = Some(video.timescale);
        track.max_gop_duration = Some(video.max_gop_duration_ms);
        tracks.push(track);
    }
    if let Some(audio) = audio {
        let mut track = MsfTrack::new("audio".to_owned(), MsfPackaging::Loc, true);
        track.namespace = Some(namespace.to_owned());
        track.role = Some("audio".to_owned());
        track.codec = Some(audio.codec.clone());
        track.samplerate = Some(audio.samplerate);
        track.channel_config = Some(audio.channel_config.clone());
        track.bitrate = Some(audio.bitrate_kbps as u64 * 1000);
        tracks.push(track);
    }
    if tracks.is_empty() {
        return Err(MoqError::Catalog(
            "catalog requires at least one track".to_owned(),
        ));
    }

    let mut catalog = MsfCatalog::new();
    catalog.is_complete = false;
    catalog.tracks = tracks;
    let document = MsfCatalogDocument::Full(catalog);
    let bytes = document
        .encode()
        .map_err(|e| MoqError::Catalog(format!("failed to encode catalog: {e}")))?;
    tracing::info!(
        target: "moq",
        catalog = %String::from_utf8_lossy(&bytes),
        "catalog encoded"
    );
    Ok(bytes)
}

/// カタログをデコードする
pub fn decode_catalog(bytes: &[u8]) -> Result<MsfCatalogDocument, MoqError> {
    MsfCatalogDocument::decode(bytes)
        .map_err(|e| MoqError::Catalog(format!("failed to decode catalog: {e}")))
}

/// codec 文字列が映像のものかどうか
///
/// draft-ietf-moq-msf-01 §5.2.18 の codec は RFC 6381 形式で、先頭が sample entry 名になる。
pub fn is_video_codec(codec: &str) -> bool {
    ["avc1", "avc3", "hvc1", "hev1", "av01"]
        .iter()
        .any(|prefix| codec.starts_with(prefix))
}

/// codec 文字列が音声のものかどうか
pub fn is_audio_codec(codec: &str) -> bool {
    codec.starts_with("opus")
}

/// カタログから映像トラックを探す
pub fn find_video_track(catalog: &MsfCatalog) -> Result<VideoTrackInfo, MoqError> {
    let track = catalog
        .tracks
        .iter()
        .find(|track| track.codec.as_deref().is_some_and(is_video_codec))
        .ok_or_else(|| MoqError::Catalog("catalog has no video track".to_owned()))?;
    let codec = track
        .codec
        .clone()
        .ok_or_else(|| MoqError::Catalog("video track has no codec".to_owned()))?;
    let width = track
        .width
        .ok_or_else(|| MoqError::Catalog("video track has no width".to_owned()))?;
    let height = track
        .height
        .ok_or_else(|| MoqError::Catalog("video track has no height".to_owned()))?;
    if width == 0 || height == 0 || width > u32::MAX as u64 || height > u32::MAX as u64 {
        return Err(MoqError::Catalog(format!(
            "video track has an invalid size: {width}x{height}"
        )));
    }
    // framerate は省略可能なため、無い場合は 30 fps として扱う
    let framerate = track
        .framerate
        .map(|framerate| framerate.round().max(1.0) as u32)
        .unwrap_or(30);
    Ok(VideoTrackInfo {
        track_name: track.name.clone().into_bytes(),
        codec,
        width: width as u32,
        height: height as u32,
        framerate,
    })
}

/// カタログから音声トラックを探す
pub fn find_audio_track(catalog: &MsfCatalog) -> Result<AudioTrackInfo, MoqError> {
    let track = catalog
        .tracks
        .iter()
        .find(|track| track.codec.as_deref().is_some_and(is_audio_codec))
        .ok_or_else(|| MoqError::Catalog("catalog has no audio track".to_owned()))?;
    let codec = track
        .codec
        .clone()
        .ok_or_else(|| MoqError::Catalog("audio track has no codec".to_owned()))?;
    let samplerate = track
        .samplerate
        .ok_or_else(|| MoqError::Catalog("audio track has no samplerate".to_owned()))?;
    if samplerate == 0 || samplerate > u32::MAX as u64 {
        return Err(MoqError::Catalog(format!(
            "audio track has an invalid samplerate: {samplerate}"
        )));
    }
    let channel_config = track
        .channel_config
        .as_deref()
        .ok_or_else(|| MoqError::Catalog("audio track has no channelConfig".to_owned()))?;
    let channels = parse_channel_config(channel_config)?;
    Ok(AudioTrackInfo {
        track_name: track.name.clone().into_bytes(),
        codec,
        samplerate: samplerate as u32,
        channels,
    })
}

/// MSF の channelConfig をチャンネル数に変換する
///
/// draft-ietf-moq-msf-01 §5.2.29 の既定の表現 (`"1"` / `"2"`) のみ扱う。
pub fn parse_channel_config(channel_config: &str) -> Result<u8, MoqError> {
    match channel_config {
        "1" => Ok(1),
        "2" => Ok(2),
        other => Err(MoqError::Catalog(format!(
            "unsupported channelConfig: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の映像トラック仕様
    fn video_spec() -> VideoTrackSpec {
        VideoTrackSpec {
            codec: "avc1.42E01E".to_owned(),
            width: 640,
            height: 480,
            framerate: 30.0,
            bitrate_kbps: 2000,
            timescale: 90000,
            max_gop_duration_ms: 2000,
        }
    }

    /// テスト用の音声トラック仕様
    fn audio_spec() -> AudioTrackSpec {
        AudioTrackSpec {
            codec: "opus".to_owned(),
            samplerate: 48000,
            channel_config: "2".to_owned(),
            bitrate_kbps: 64,
        }
    }

    /// 映像と音声のカタログを生成してデコードし直せること
    #[test]
    fn test_encode_and_decode_catalog() {
        let bytes = encode_catalog("momo", Some(&video_spec()), Some(&audio_spec()))
            .expect("エンコードに成功すること");
        let document = decode_catalog(&bytes).expect("デコードに成功すること");
        let MsfCatalogDocument::Full(catalog) = document else {
            panic!("完全カタログが返ること");
        };
        assert_eq!(catalog.tracks.len(), 2);
        let video = find_video_track(&catalog).expect("映像トラックが見つかること");
        assert_eq!(video.track_name, b"video");
        assert_eq!(video.codec, "avc1.42E01E");
        assert_eq!(video.width, 640);
        assert_eq!(video.height, 480);
        assert_eq!(video.framerate, 30);
        let audio = find_audio_track(&catalog).expect("音声トラックが見つかること");
        assert_eq!(audio.track_name, b"audio");
        assert_eq!(audio.codec, "opus");
        assert_eq!(audio.samplerate, 48000);
        assert_eq!(audio.channels, 2);
    }

    /// トラックが無い場合はエラーになること
    #[test]
    fn test_encode_catalog_rejects_no_tracks() {
        assert!(encode_catalog("momo", None, None).is_err());
    }

    /// 映像トラックが無いカタログでは映像の探索がエラーになること
    #[test]
    fn test_find_video_track_rejects_missing_track() {
        let bytes =
            encode_catalog("momo", None, Some(&audio_spec())).expect("エンコードに成功すること");
        let MsfCatalogDocument::Full(catalog) =
            decode_catalog(&bytes).expect("デコードに成功すること")
        else {
            panic!("完全カタログが返ること");
        };
        assert!(find_video_track(&catalog).is_err());
    }

    /// codec の接頭辞で映像を判定できること
    #[test]
    fn test_is_video_codec() {
        assert!(is_video_codec("avc1.42E01E"));
        assert!(is_video_codec("avc3.640028"));
        assert!(is_video_codec("hvc1.1.6.L93.B0"));
        assert!(is_video_codec("hev1.1.6.L93.B0"));
        assert!(is_video_codec("av01.0.08M.08"));
        assert!(!is_video_codec("opus"));
    }

    /// codec の接頭辞で音声を判定できること
    #[test]
    fn test_is_audio_codec() {
        assert!(is_audio_codec("opus"));
        assert!(!is_audio_codec("avc1.42E01E"));
    }

    /// channelConfig をチャンネル数に変換できること
    #[test]
    fn test_parse_channel_config() {
        assert_eq!(parse_channel_config("1").expect("変換に成功すること"), 1);
        assert_eq!(parse_channel_config("2").expect("変換に成功すること"), 2);
        assert!(parse_channel_config("3").is_err());
    }
}
