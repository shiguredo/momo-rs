//! MOQT モードの設定と引数の検証
//!
//! `momo sora-moq publish` / `momo sora-moq subscribe` の設定を保持し、
//! 起動時に指定できる値の範囲を検証する。

/// 映像ビットレートの下限 (kbps)
///
/// 0 は Sora モードで「指定なし」を意味するため、sora-moq では拒否する。
pub const MIN_VIDEO_BIT_RATE: u32 = 1;
/// 映像ビットレートの上限 (kbps)
pub const MAX_VIDEO_BIT_RATE: u32 = 30000;
/// 音声ビットレートの下限 (kbps)
pub const MIN_AUDIO_BIT_RATE: u32 = 1;
/// 音声ビットレートの上限 (kbps)
pub const MAX_AUDIO_BIT_RATE: u32 = 510;
/// キーフレーム間隔の既定値 (フレーム数)
pub const DEFAULT_VIDEO_KEYFRAME_INTERVAL: u32 = 60;
/// キーフレーム間隔の下限 (フレーム数)
pub const MIN_VIDEO_KEYFRAME_INTERVAL: u32 = 1;
/// キーフレーム間隔の上限 (フレーム数)
///
/// 1 時間ぶんに相当する値。これを超える値は入力ミスの可能性が高い。
pub const MAX_VIDEO_KEYFRAME_INTERVAL: u32 = 3600 * 120;

/// MOQT モードのロール
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoqRole {
    /// トラックを publish する側
    Publish,
    /// トラックを subscribe する側
    Subscribe,
}

/// `momo sora-moq` のサブコマンド引数から作る設定
#[derive(Debug, Clone)]
pub struct MoqConfig {
    /// ロール
    pub role: MoqRole,
    /// 接続先 URL (`moqt://host:port/path`)
    pub url: String,
    /// Track Namespace
    pub namespace: String,
    /// 映像トラックを publish するかどうか (`Publish` でのみ使う)
    pub video: bool,
    /// 音声トラックを publish するかどうか (`Publish` でのみ使う)
    pub audio: bool,
    /// 映像ビットレート (kbps)。映像が無効な場合は使わない
    pub video_bit_rate: Option<u32>,
    /// 音声ビットレート (kbps)。音声が無効な場合は使わない
    pub audio_bit_rate: Option<u32>,
    /// キーフレーム間隔 (フレーム数)
    pub video_keyframe_interval: u32,
}

impl MoqConfig {
    /// 設定を検証する
    ///
    /// 次の条件を満たさない場合はエラーを返す。
    ///
    /// - `--url` と `--namespace` が指定されている
    /// - キーフレーム間隔が範囲内である
    /// - publish では映像と音声の少なくとも一方が有効である
    /// - publish で有効なトラックのビットレートが指定され、範囲内である
    pub fn validate(&self) -> Result<(), String> {
        if self.url.is_empty() {
            return Err("missing '--url' option".to_owned());
        }
        if self.namespace.is_empty() {
            return Err("missing '--namespace' option".to_owned());
        }
        if !(MIN_VIDEO_KEYFRAME_INTERVAL..=MAX_VIDEO_KEYFRAME_INTERVAL)
            .contains(&self.video_keyframe_interval)
        {
            return Err(format!(
                "--video-keyframe-interval must be in {MIN_VIDEO_KEYFRAME_INTERVAL}-{MAX_VIDEO_KEYFRAME_INTERVAL} frames, but {}",
                self.video_keyframe_interval
            ));
        }
        // subscribe ではビットレートを指定しないため、引数の検証は publish だけを対象にする
        if self.role != MoqRole::Publish {
            return Ok(());
        }
        if !self.video && !self.audio {
            return Err(
                "--no-video-input-device and --no-audio-device cannot be used at the same time"
                    .to_owned(),
            );
        }
        if self.video {
            match self.video_bit_rate {
                Some(rate) => {
                    if !(MIN_VIDEO_BIT_RATE..=MAX_VIDEO_BIT_RATE).contains(&rate) {
                        return Err(format!(
                            "--video-bit-rate must be in {MIN_VIDEO_BIT_RATE}-{MAX_VIDEO_BIT_RATE} kbps, but {rate}"
                        ));
                    }
                }
                None => return Err("missing '--video-bit-rate' option".to_owned()),
            }
        }
        if self.audio {
            match self.audio_bit_rate {
                Some(rate) => {
                    if !(MIN_AUDIO_BIT_RATE..=MAX_AUDIO_BIT_RATE).contains(&rate) {
                        return Err(format!(
                            "--audio-bit-rate must be in {MIN_AUDIO_BIT_RATE}-{MAX_AUDIO_BIT_RATE} kbps, but {rate}"
                        ));
                    }
                }
                None => return Err("missing '--audio-bit-rate' option".to_owned()),
            }
        }
        Ok(())
    }
}

/// `MomoConfig` から sora-moq に渡す共通設定
///
/// グローバルオプションで指定される入力デバイスや解像度などと、
/// TLS の設定をまとめて受け取る。
#[derive(Debug, Clone)]
pub struct MoqCommonConfig {
    /// サーバー証明書の検証をスキップするかどうか (`--insecure`)
    pub insecure: bool,
    /// CA 証明書 (PEM の内容)。指定した場合はこの証明書で検証する (`--cacert`)
    pub ca_cert: Option<String>,
    /// 映像の幅 (`--resolution`)
    pub video_width: i32,
    /// 映像の高さ (`--resolution`)
    pub video_height: i32,
    /// フレームレート (`--framerate`)
    pub framerate: u32,
    /// 映像トラックを publish しない (`--no-video-input-device`)
    pub no_video_input_device: bool,
    /// 音声トラックを publish しない (`--no-audio-device`)
    pub no_audio_device: bool,
    /// 疑似映像キャプチャを使うかどうか (`--fake-capture-device`)
    pub fake_capture_device: bool,
    /// 映像入力デバイス (`--video-input-device`)
    pub video_input_device: Option<String>,
    /// 音声入力デバイス (`--audio-input-device`)
    pub audio_input_device: Option<String>,
    /// OpenH264 ライブラリ (`--openh264`)
    pub openh264_lib: Option<shiguredo_openh264::Openh264Library>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 有効な publish の設定を構築する
    fn valid_config() -> MoqConfig {
        MoqConfig {
            role: MoqRole::Publish,
            url: "moqt://example.com:4433/live".to_owned(),
            namespace: "momo".to_owned(),
            video: true,
            audio: true,
            video_bit_rate: Some(2000),
            audio_bit_rate: Some(64),
            video_keyframe_interval: DEFAULT_VIDEO_KEYFRAME_INTERVAL,
        }
    }

    /// 有効な設定が検証を通ること
    #[test]
    fn test_validate_accepts_valid_config() {
        assert!(valid_config().validate().is_ok());
    }

    /// --url が無い場合にエラーになること
    #[test]
    fn test_validate_rejects_missing_url() {
        let mut config = valid_config();
        config.url = String::new();
        assert_eq!(
            config.validate().expect_err("検証に失敗すること"),
            "missing '--url' option"
        );
    }

    /// --namespace が無い場合にエラーになること
    #[test]
    fn test_validate_rejects_missing_namespace() {
        let mut config = valid_config();
        config.namespace = String::new();
        assert_eq!(
            config.validate().expect_err("検証に失敗すること"),
            "missing '--namespace' option"
        );
    }

    /// 映像と音声の両方が無効な場合にエラーになること
    #[test]
    fn test_validate_rejects_no_media() {
        let mut config = valid_config();
        config.video = false;
        config.audio = false;
        assert!(
            config
                .validate()
                .expect_err("検証に失敗すること")
                .starts_with("--no-video-input-device and --no-audio-device")
        );
    }

    /// subscribe では映像と音声が無効でもエラーにならないこと
    #[test]
    fn test_validate_accepts_subscribe_without_media() {
        let mut config = valid_config();
        config.role = MoqRole::Subscribe;
        config.video = false;
        config.audio = false;
        config.video_bit_rate = None;
        config.audio_bit_rate = None;
        assert!(config.validate().is_ok());
    }

    /// 映像が無効な場合は映像ビットレートを要求しないこと
    #[test]
    fn test_validate_accepts_missing_video_bit_rate_when_video_disabled() {
        let mut config = valid_config();
        config.video = false;
        config.video_bit_rate = None;
        assert!(config.validate().is_ok());
    }

    /// 音声が無効な場合は音声ビットレートを要求しないこと
    #[test]
    fn test_validate_accepts_missing_audio_bit_rate_when_audio_disabled() {
        let mut config = valid_config();
        config.audio = false;
        config.audio_bit_rate = None;
        assert!(config.validate().is_ok());
    }

    /// 映像が有効で映像ビットレートが無い場合にエラーになること
    #[test]
    fn test_validate_rejects_missing_video_bit_rate() {
        let mut config = valid_config();
        config.video_bit_rate = None;
        assert_eq!(
            config.validate().expect_err("検証に失敗すること"),
            "missing '--video-bit-rate' option"
        );
    }

    /// 音声が有効で音声ビットレートが無い場合にエラーになること
    #[test]
    fn test_validate_rejects_missing_audio_bit_rate() {
        let mut config = valid_config();
        config.audio_bit_rate = None;
        assert_eq!(
            config.validate().expect_err("検証に失敗すること"),
            "missing '--audio-bit-rate' option"
        );
    }

    /// 映像ビットレートの 0 を拒否すること
    #[test]
    fn test_validate_rejects_zero_video_bit_rate() {
        let mut config = valid_config();
        config.video_bit_rate = Some(0);
        assert!(
            config
                .validate()
                .expect_err("検証に失敗すること")
                .starts_with("--video-bit-rate must be in 1-30000")
        );
    }

    /// 音声ビットレートの 0 を拒否すること
    #[test]
    fn test_validate_rejects_zero_audio_bit_rate() {
        let mut config = valid_config();
        config.audio_bit_rate = Some(0);
        assert!(
            config
                .validate()
                .expect_err("検証に失敗すること")
                .starts_with("--audio-bit-rate must be in 1-510")
        );
    }

    /// 映像ビットレートの上限を超える値を拒否すること
    #[test]
    fn test_validate_rejects_excessive_video_bit_rate() {
        let mut config = valid_config();
        config.video_bit_rate = Some(MAX_VIDEO_BIT_RATE + 1);
        assert!(config.validate().is_err());
    }

    /// 音声ビットレートの上限を超える値を拒否すること
    #[test]
    fn test_validate_rejects_excessive_audio_bit_rate() {
        let mut config = valid_config();
        config.audio_bit_rate = Some(MAX_AUDIO_BIT_RATE + 1);
        assert!(config.validate().is_err());
    }

    /// 映像ビットレートの上限が通ること
    #[test]
    fn test_validate_accepts_max_video_bit_rate() {
        let mut config = valid_config();
        config.video_bit_rate = Some(MAX_VIDEO_BIT_RATE);
        assert!(config.validate().is_ok());
    }

    /// 音声ビットレートの上限が通ること
    #[test]
    fn test_validate_accepts_max_audio_bit_rate() {
        let mut config = valid_config();
        config.audio_bit_rate = Some(MAX_AUDIO_BIT_RATE);
        assert!(config.validate().is_ok());
    }

    /// キーフレーム間隔の 0 を拒否すること
    #[test]
    fn test_validate_rejects_zero_keyframe_interval() {
        let mut config = valid_config();
        config.video_keyframe_interval = 0;
        assert!(
            config
                .validate()
                .expect_err("検証に失敗すること")
                .starts_with("--video-keyframe-interval must be in 1-")
        );
    }

    /// キーフレーム間隔の上限を超える値を拒否すること
    #[test]
    fn test_validate_rejects_excessive_keyframe_interval() {
        let mut config = valid_config();
        config.video_keyframe_interval = MAX_VIDEO_KEYFRAME_INTERVAL + 1;
        assert!(config.validate().is_err());
    }

    /// subscribe でもキーフレーム間隔の範囲は検証されること
    #[test]
    fn test_validate_rejects_zero_keyframe_interval_for_subscribe() {
        let mut config = valid_config();
        config.role = MoqRole::Subscribe;
        config.video_keyframe_interval = 0;
        assert!(config.validate().is_err());
    }
}
