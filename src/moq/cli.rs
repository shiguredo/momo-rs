//! MOQT (Media over QUIC Transport) モードの CLI
//!
//! `momo sora-moq publish` / `momo sora-moq subscribe` のサブコマンドを提供する。

use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use crate::moq::config::{DEFAULT_VIDEO_KEYFRAME_INTERVAL, MoqCommonConfig, MoqConfig, MoqRole};
use crate::moq::{publisher, subscriber};

/// `momo sora-moq` サブコマンドを実行する
///
/// 第 1 サブコマンドで役割 (publish / subscribe) を選ぶ。
pub async fn run(mut args: noargs::RawArgs, common: MoqCommonConfig) -> noargs::Result<()> {
    noargs::HELP_FLAG.take_help(&mut args);

    if noargs::cmd("publish")
        .doc("Publish MOQT tracks")
        .take(&mut args)
        .is_present()
    {
        run_publish(args, common).await
    } else if noargs::cmd("subscribe")
        .doc("Subscribe to MOQT tracks and play them")
        .take(&mut args)
        .is_present()
    {
        run_subscribe(args, common).await
    } else if let Some(help) = args.finish()? {
        print!("{}", help);
        Ok(())
    } else {
        Err(noargs::Error::other(
            &noargs::raw_args(),
            "sora-moq mode requires a subcommand: publish or subscribe",
        ))
    }
}

/// 数値オプションをパースする
fn parse_u32(value: Option<String>, name: &str) -> noargs::Result<Option<u32>> {
    match value {
        Some(value) => value.parse::<u32>().map(Some).map_err(|_| {
            noargs::Error::other(
                &noargs::raw_args(),
                format!("invalid value for '--{name}': {value}"),
            )
        }),
        None => Ok(None),
    }
}

/// 設定を検証する
fn validate(config: &MoqConfig) -> noargs::Result<()> {
    config
        .validate()
        .map_err(|e| noargs::Error::other(&noargs::raw_args(), e))
}

/// `--video` / `--audio` を読み捨てる
///
/// sora-moq ではトラックの有無を `--no-video-input-device` / `--no-audio-device` で
/// 決めるため、Sora モードの `--video` / `--audio` は指定されても無視する。
fn take_ignored_track_options(args: &mut noargs::RawArgs) {
    let _ = noargs::opt("video")
        .ty("BOOL")
        .doc("Ignored in sora-moq mode")
        .take(args)
        .present();
    let _ = noargs::opt("audio")
        .ty("BOOL")
        .doc("Ignored in sora-moq mode")
        .take(args)
        .present();
}

/// `momo sora-moq publish` を実行する
async fn run_publish(mut args: noargs::RawArgs, common: MoqCommonConfig) -> noargs::Result<()> {
    noargs::HELP_FLAG.take_help(&mut args);

    // オプションを宣言してからヘルプを判定する。noargs は宣言済みのオプションだけを
    // ヘルプに載せるため、先に return するとオプション一覧が空になる。
    // 値の必須判定は help_mode を見てから行う (take はヘルプ要求時にも値を要求するため、
    // ここでは present() で有無だけを見る)
    let url = noargs::opt("url")
        .ty("URL")
        .doc("MOQT relay URL (moqt://host:port/path)")
        .take(&mut args)
        .present()
        .map(|o| o.value().to_owned());
    let namespace = noargs::opt("namespace")
        .ty("NAMESPACE")
        .doc("Track Namespace")
        .take(&mut args)
        .present()
        .map(|o| o.value().to_owned());
    let video_bit_rate = noargs::opt("video-bit-rate")
        .ty("KBPS")
        .doc("Video bit rate in kbps (1-30000)")
        .take(&mut args)
        .present()
        .map(|o| o.value().to_owned());
    let audio_bit_rate = noargs::opt("audio-bit-rate")
        .ty("KBPS")
        .doc("Audio bit rate in kbps (1-510)")
        .take(&mut args)
        .present()
        .map(|o| o.value().to_owned());
    let video_keyframe_interval = noargs::opt("video-keyframe-interval")
        .ty("FRAMES")
        .doc("Keyframe interval in frames")
        .take(&mut args)
        .present()
        .map(|o| o.value().to_owned());
    take_ignored_track_options(&mut args);

    if let Some(help) = args.finish()? {
        print!("{}", help);
        return Ok(());
    }

    let config = MoqConfig {
        role: MoqRole::Publish,
        url: url.unwrap_or_default(),
        namespace: namespace.unwrap_or_default(),
        video: !common.no_video_input_device,
        audio: !common.no_audio_device,
        video_bit_rate: parse_u32(video_bit_rate, "video-bit-rate")?,
        audio_bit_rate: parse_u32(audio_bit_rate, "audio-bit-rate")?,
        video_keyframe_interval: parse_u32(video_keyframe_interval, "video-keyframe-interval")?
            .unwrap_or(DEFAULT_VIDEO_KEYFRAME_INTERVAL),
    };
    validate(&config)?;

    publisher::run(config, common)
        .await
        .map_err(|e| noargs::Error::other(&noargs::raw_args(), e.to_string()))
}

/// `momo sora-moq subscribe` を実行する
async fn run_subscribe(mut args: noargs::RawArgs, common: MoqCommonConfig) -> noargs::Result<()> {
    noargs::HELP_FLAG.take_help(&mut args);

    let url = noargs::opt("url")
        .ty("URL")
        .doc("MOQT relay URL (moqt://host:port/path)")
        .take(&mut args)
        .present()
        .map(|o| o.value().to_owned());
    let namespace = noargs::opt("namespace")
        .ty("NAMESPACE")
        .doc("Track Namespace")
        .take(&mut args)
        .present()
        .map(|o| o.value().to_owned());
    take_ignored_track_options(&mut args);

    if let Some(help) = args.finish()? {
        print!("{}", help);
        return Ok(());
    }

    // subscribe はカタログにある映像と音声の両方を購読する
    let config = MoqConfig {
        role: MoqRole::Subscribe,
        url: url.unwrap_or_default(),
        namespace: namespace.unwrap_or_default(),
        video: true,
        audio: true,
        video_bit_rate: None,
        audio_bit_rate: None,
        video_keyframe_interval: DEFAULT_VIDEO_KEYFRAME_INTERVAL,
    };
    validate(&config)?;

    // 復号はバックグラウンドのタスクで行い、再生はメインスレッドで行う
    // (macOS では SDL のウィンドウ操作をメインスレッドで行う必要がある)
    let (video_tx, video_rx) = std::sync::mpsc::channel();
    let (audio_tx, audio_rx) = std::sync::mpsc::channel();
    let backlog = Arc::new(AtomicI64::new(0));
    let pipeline = tokio::spawn(subscriber::run(
        config,
        common,
        video_tx,
        audio_tx,
        Arc::clone(&backlog),
    ));

    let player_result =
        tokio::task::block_in_place(|| subscriber::run_player(video_rx, audio_rx, backlog));
    if pipeline.is_finished() {
        match pipeline.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return Err(noargs::Error::other(&noargs::raw_args(), e.to_string()));
            }
            Err(e) if e.is_cancelled() => {}
            Err(e) => {
                return Err(noargs::Error::other(&noargs::raw_args(), e.to_string()));
            }
        }
    } else {
        pipeline.abort();
    }
    player_result.map_err(|e| noargs::Error::other(&noargs::raw_args(), e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moq::config::{MAX_VIDEO_BIT_RATE, MIN_VIDEO_BIT_RATE};

    /// 映像ビットレートが範囲外の場合にエラーになること
    #[test]
    fn test_validate_rejects_video_bit_rate_out_of_range() {
        let config = MoqConfig {
            role: MoqRole::Publish,
            url: "moqt://example.com:4433/live".to_owned(),
            namespace: "momo".to_owned(),
            video: true,
            audio: false,
            video_bit_rate: Some(MAX_VIDEO_BIT_RATE + 1),
            audio_bit_rate: None,
            video_keyframe_interval: DEFAULT_VIDEO_KEYFRAME_INTERVAL,
        };
        assert!(validate(&config).is_err());
    }

    /// 映像と音声の同時無効がエラーになること
    #[test]
    fn test_validate_rejects_both_tracks_disabled() {
        let config = MoqConfig {
            role: MoqRole::Publish,
            url: "moqt://example.com:4433/live".to_owned(),
            namespace: "momo".to_owned(),
            video: false,
            audio: false,
            video_bit_rate: None,
            audio_bit_rate: None,
            video_keyframe_interval: DEFAULT_VIDEO_KEYFRAME_INTERVAL,
        };
        assert!(validate(&config).is_err());
    }

    /// subscribe ではビットレート無しで検証を通ること
    #[test]
    fn test_validate_accepts_subscribe() {
        let config = MoqConfig {
            role: MoqRole::Subscribe,
            url: "moqt://example.com:4433/live".to_owned(),
            namespace: "momo".to_owned(),
            video: true,
            audio: true,
            video_bit_rate: None,
            audio_bit_rate: None,
            video_keyframe_interval: DEFAULT_VIDEO_KEYFRAME_INTERVAL,
        };
        assert!(validate(&config).is_ok());
    }

    /// 数値オプションのパースに失敗した場合にエラーになること
    #[test]
    fn test_parse_u32_rejects_invalid_value() {
        assert!(parse_u32(Some("abc".to_owned()), "video-bit-rate").is_err());
        assert_eq!(
            parse_u32(Some("100".to_owned()), "video-bit-rate").expect("パースに成功すること"),
            Some(100)
        );
        assert_eq!(
            parse_u32(None, "video-bit-rate").expect("パースに成功すること"),
            None
        );
    }

    /// 映像ビットレートの下限が通ること
    #[test]
    fn test_validate_accepts_min_video_bit_rate() {
        let config = MoqConfig {
            role: MoqRole::Publish,
            url: "moqt://example.com:4433/live".to_owned(),
            namespace: "momo".to_owned(),
            video: true,
            audio: false,
            video_bit_rate: Some(MIN_VIDEO_BIT_RATE),
            audio_bit_rate: None,
            video_keyframe_interval: DEFAULT_VIDEO_KEYFRAME_INTERVAL,
        };
        assert!(validate(&config).is_ok());
    }
}
