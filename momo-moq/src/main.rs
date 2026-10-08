//! MOQT (Media over QUIC Transport) クライアント
//!
//! `draft-ietf-moq-transport-21` に基づき、MOQT relay へトラックを publish または
//! subscribe する。使い方は README を参照すること。

mod audio_device;
mod catalog;
mod cli;
mod client;
mod config;
mod error;
mod h264;
mod media;
mod opus;
mod publisher;
mod quic;
mod subscriber;
mod transport;
mod url;

#[tokio::main]
async fn main() -> noargs::Result<()> {
    let mut args = noargs::raw_args();
    args.metadata_mut().app_name = env!("CARGO_PKG_NAME");
    args.metadata_mut().app_description = env!("CARGO_PKG_DESCRIPTION");

    if noargs::VERSION_FLAG.take(&mut args).is_present() {
        println!(
            "{} {} ({})",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            env!("MOMO_COMMIT_SHORT"),
        );
        println!("OpenH264: {} (build)", shiguredo_openh264::BUILD_VERSION);
        return Ok(());
    }

    noargs::HELP_FLAG.take_help(&mut args);

    // ── グローバルフラグ ──
    let no_video_input_device = noargs::flag("no-video-input-device")
        .doc("Do not publish the video track")
        .take(&mut args)
        .is_present();
    let no_audio_device = noargs::flag("no-audio-device")
        .doc("Do not publish the audio track")
        .take(&mut args)
        .is_present();
    let fake_capture_device = noargs::flag("fake-capture-device")
        .doc("Use fake video and audio capture devices")
        .take(&mut args)
        .is_present();
    let insecure = noargs::flag("insecure")
        .doc("Allow insecure server connections when using SSL")
        .take(&mut args)
        .is_present();

    // ── グローバルオプション ──
    let video_input_device: Option<String> = noargs::opt("video-input-device")
        .ty("DEVICE")
        .doc("Use the video device specified by an index or a name (use the first one if not specified)")
        .take(&mut args)
        .present_and_then(|o| o.value().parse())?;
    let audio_input_device: Option<String> = noargs::opt("audio-input-device")
        .ty("DEVICE")
        .doc("Use the audio input device specified by an index or a name (use the system default if not specified)")
        .take(&mut args)
        .present_and_then(|o| o.value().parse())?;
    let resolution: String = noargs::opt("resolution")
        .ty("RESOLUTION")
        .doc("Video resolution (one of QVGA, VGA, HD, FHD, 4K, or [WIDTH]x[HEIGHT])")
        .default("VGA")
        .take(&mut args)
        .then(|o| o.value().parse())?;
    let (video_width, video_height) =
        parse_resolution(&resolution).map_err(|e| noargs::Error::other(&noargs::raw_args(), e))?;
    let framerate: String = noargs::opt("framerate")
        .ty("FRAMERATE")
        .doc("Video framerate (1-120)")
        .default("30")
        .take(&mut args)
        .then(|o| o.value().parse())?;
    let framerate: u32 =
        parse_framerate(&framerate).map_err(|e| noargs::Error::other(&noargs::raw_args(), e))?;
    let log_level: Option<String> = noargs::opt("log-level")
        .ty("LEVEL")
        .doc("Log severity level threshold (verbose, info, warning, error, none)")
        .take(&mut args)
        .present_and_then(|o| o.value().parse())?;
    let cacert: Option<String> = noargs::opt("cacert")
        .ty("PATH")
        .doc("CA certificate file path (PEM format)")
        .take(&mut args)
        .present_and_then(|o| o.value().parse())?;
    let openh264_path: Option<String> = noargs::opt("openh264")
        .ty("PATH")
        .doc("OpenH264 dynamic library path")
        .take(&mut args)
        .present_and_then(|o| o.value().parse())?;

    // ── CA 証明書の読み込み ──
    let ca_cert_pem = match &cacert {
        Some(path) => {
            let pem = std::fs::read_to_string(path).map_err(|e| {
                noargs::Error::other(
                    &noargs::raw_args(),
                    format!("cacert file read error: {path}: {e}"),
                )
            })?;
            Some(pem)
        }
        None => None,
    };

    // ── OpenH264 ライブラリのロード ──
    let openh264_lib = match &openh264_path {
        Some(path) => {
            let lib = shiguredo_openh264::Openh264Library::load(std::path::Path::new(path))
                .map_err(|e| {
                    noargs::Error::other(
                        &noargs::raw_args(),
                        format!("OpenH264 library load failed: {e}"),
                    )
                })?;
            Some(lib)
        }
        None => None,
    };

    // ── ログ初期化 ──
    init_tracing(log_level.as_deref());

    let common = config::MoqCommonConfig {
        insecure,
        ca_cert: ca_cert_pem,
        video_width,
        video_height,
        framerate,
        no_video_input_device,
        no_audio_device,
        fake_capture_device,
        video_input_device,
        audio_input_device,
        openh264_lib,
    };

    cli::run(args, common).await
}

/// 解像度文字列を (width, height) にパースする
fn parse_resolution(s: &str) -> Result<(i32, i32), String> {
    match s.to_uppercase().as_str() {
        "QVGA" => Ok((320, 240)),
        "VGA" => Ok((640, 480)),
        "HD" => Ok((1280, 720)),
        "FHD" => Ok((1920, 1080)),
        "4K" => Ok((3840, 2160)),
        _ => {
            // WxH 形式
            let parts: Vec<&str> = s.split('x').collect();
            if parts.len() != 2 {
                return Err(format!(
                    "invalid resolution: '{}' (expected QVGA, VGA, HD, FHD, 4K, or WIDTHxHEIGHT)",
                    s
                ));
            }
            let w: i32 = parts[0]
                .parse()
                .map_err(|_| format!("invalid width in resolution: '{}'", parts[0]))?;
            let h: i32 = parts[1]
                .parse()
                .map_err(|_| format!("invalid height in resolution: '{}'", parts[1]))?;
            if w <= 0 || h <= 0 {
                return Err(format!("resolution must be positive: {}x{}", w, h));
            }
            Ok((w, h))
        }
    }
}

/// `--framerate` の下限値 (momo と同じ)
const MIN_FRAMERATE: u32 = 1;
/// `--framerate` の上限値 (momo と同じ)
const MAX_FRAMERATE: u32 = 120;

/// `--framerate` の値を範囲検証付きでパースする
///
/// 0 はフェイク映像スレッドのゼロ除算に、上限超は `frame_duration` が 0 ナノ秒に丸まって
/// ビジーループになる。デバイスやスレッドを起動する前に失敗させる
fn parse_framerate(value: &str) -> Result<u32, String> {
    let parsed: u32 = value.parse().map_err(|_| {
        format!(
            "invalid framerate: '{value}' ({MIN_FRAMERATE}-{MAX_FRAMERATE} の整数を指定してください)"
        )
    })?;
    if !(MIN_FRAMERATE..=MAX_FRAMERATE).contains(&parsed) {
        return Err(format!(
            "invalid framerate: {parsed} ({MIN_FRAMERATE}-{MAX_FRAMERATE} の範囲を指定してください)"
        ));
    }
    Ok(parsed)
}

/// tracing_subscriber を初期化する
///
/// `--log-level` の値を tracing のフィルターに変換する。
/// verbose=TRACE, info=INFO, warning=WARN, error=ERROR, none=OFF。
/// 未指定時は INFO。
fn init_tracing(log_level: Option<&str>) {
    use tracing_subscriber::EnvFilter;

    let filter = match log_level {
        Some("verbose") => "trace",
        Some("info") | None => "info",
        Some("warning") => "warn",
        Some("error") => "error",
        Some("none") => "off",
        Some(other) => {
            eprintln!("unknown log level: '{}', using 'info'", other);
            "info"
        }
    };

    tracing_subscriber::fmt()
        .compact()
        .with_env_filter(EnvFilter::new(filter))
        .init();
}

#[cfg(test)]
mod tests {
    use super::{MAX_FRAMERATE, MIN_FRAMERATE, parse_framerate, parse_resolution};

    /// momo と同じ範囲 (1〜120) が受理されること
    #[test]
    fn parse_framerate_accepts_momo_range() {
        for fps in [MIN_FRAMERATE, 24, 30, 60, MAX_FRAMERATE] {
            let parsed = parse_framerate(&fps.to_string());
            assert_eq!(parsed.ok(), Some(fps), "{fps} は受理するはず");
        }
    }

    /// 範囲外と非数値は起動時に拒否されること
    #[test]
    fn parse_framerate_rejects_invalid_values() {
        for value in ["0", "121", "1000", "-1", "abc", "", " 30"] {
            let parsed = parse_framerate(value);
            assert!(parsed.is_err(), "{value:?} は拒否するはず");
        }
    }

    /// 解像度のプリセットと WxH 形式がパースできること
    #[test]
    fn parse_resolution_accepts_presets_and_size() {
        assert_eq!(parse_resolution("VGA"), Ok((640, 480)));
        assert_eq!(parse_resolution("hd"), Ok((1280, 720)));
        assert_eq!(parse_resolution("1920x1080"), Ok((1920, 1080)));
    }

    /// 不正な解像度は起動時に拒否されること
    #[test]
    fn parse_resolution_rejects_invalid_values() {
        for value in ["", "VGAx", "0x100", "100x0", "abc", "1x2x3"] {
            assert!(parse_resolution(value).is_err(), "{value:?} は拒否するはず");
        }
    }
}
