//! P2P / Ayame モード用のビデオコーデックファクトリ
//!
//! sora_sdk の `VideoCodecCapability` を利用し、`--{codec}-encoder` /
//! `--{codec}-decoder` で指定された実装を選択する `VideoEncoderFactory` /
//! `VideoDecoderFactory` を構築する。Sora モードと同じ capability を使うため、
//! nvcodec / vpl / amf / VideoToolbox のハードウェア実装を P2P / Ayame でも
//! 利用できる。

/// コーデックエンコーダー/デコーダー選択の指定
///
/// 各値は `default` / `software` / `nvidia` / `vpl` / `amf` / `videotoolbox` の
/// いずれか。`None` の場合はビルトイン (software) を使う。
#[derive(Debug, Default, Clone)]
pub(crate) struct CodecSelection {
    pub vp8_encoder: Option<String>,
    pub vp8_decoder: Option<String>,
    pub vp9_encoder: Option<String>,
    pub vp9_decoder: Option<String>,
    pub av1_encoder: Option<String>,
    pub av1_decoder: Option<String>,
    pub h264_encoder: Option<String>,
    pub h264_decoder: Option<String>,
    pub h265_encoder: Option<String>,
    pub h265_decoder: Option<String>,
}

impl CodecSelection {
    /// すべてのオプションが未指定かどうかを返す
    pub(crate) fn is_empty(&self) -> bool {
        self.vp8_encoder.is_none()
            && self.vp8_decoder.is_none()
            && self.vp9_encoder.is_none()
            && self.vp9_decoder.is_none()
            && self.av1_encoder.is_none()
            && self.av1_decoder.is_none()
            && self.h264_encoder.is_none()
            && self.h264_decoder.is_none()
            && self.h265_encoder.is_none()
            && self.h265_decoder.is_none()
    }
}

mod sora_factory {
    use std::sync::{Arc, Mutex};

    use shiguredo_webrtc::{
        EnvironmentRef, SdpVideoFormat, SdpVideoFormatRef, VideoCodecType, VideoDecoder,
        VideoDecoderFactory, VideoDecoderFactoryHandler, VideoEncoder, VideoEncoderFactory,
        VideoEncoderFactoryHandler,
    };
    use sora_sdk::{
        CodecDirection, InternalVideoCodecCapability, VideoCodecCapability,
        VideoCodecImplementation, VideoCodecPreference,
    };
    use tracing::info;

    use super::CodecSelection;
    use crate::error::BoxError;

    /// capability をエンコーダー/デコーダーファクトリで共有するための型
    type Capabilities = Arc<Mutex<Vec<Box<dyn VideoCodecCapability>>>>;

    /// 選択された実装を使うビデオエンコーダーファクトリ
    struct SelectedVideoEncoderFactory {
        preference: VideoCodecPreference,
        capabilities: Capabilities,
    }

    /// 選択された実装を使うビデオデコーダーファクトリ
    struct SelectedVideoDecoderFactory {
        preference: VideoCodecPreference,
        capabilities: Capabilities,
    }

    impl VideoEncoderFactoryHandler for SelectedVideoEncoderFactory {
        fn get_supported_formats(&mut self) -> Vec<SdpVideoFormat> {
            let capabilities = self
                .capabilities
                .lock()
                .expect("capabilities should not be poisoned");
            collect_supported_formats(
                &self.preference,
                capabilities.as_slice(),
                CodecDirection::Encoder,
            )
        }

        fn create(
            &mut self,
            env: EnvironmentRef<'_>,
            format: SdpVideoFormatRef<'_>,
        ) -> Option<VideoEncoder> {
            let capabilities = self
                .capabilities
                .lock()
                .expect("capabilities should not be poisoned");
            create_encoder(&self.preference, capabilities.as_slice(), env, format)
        }
    }

    impl VideoDecoderFactoryHandler for SelectedVideoDecoderFactory {
        fn get_supported_formats(&mut self) -> Vec<SdpVideoFormat> {
            let capabilities = self
                .capabilities
                .lock()
                .expect("capabilities should not be poisoned");
            collect_supported_formats(
                &self.preference,
                capabilities.as_slice(),
                CodecDirection::Decoder,
            )
        }

        fn create(
            &mut self,
            env: EnvironmentRef<'_>,
            format: SdpVideoFormatRef<'_>,
        ) -> Option<VideoDecoder> {
            let capabilities = self
                .capabilities
                .lock()
                .expect("capabilities should not be poisoned");
            create_decoder(&self.preference, capabilities.as_slice(), env, format)
        }
    }

    /// P2P / Ayame モードで使うエンコーダー/デコーダーファクトリを構築する
    pub(crate) fn build_factories(
        selection: &CodecSelection,
    ) -> Result<(VideoEncoderFactory, VideoDecoderFactory), BoxError> {
        let mut capabilities: Vec<Box<dyn VideoCodecCapability>> = Vec::new();

        // ビルトイン (software) は常に登録する
        let internal = InternalVideoCodecCapability::new();
        let mut preference = VideoCodecPreference::new_from_capability(&internal);
        capabilities.push(Box::new(internal));

        // ハードウェア capability を feature に応じて登録する
        #[cfg(feature = "nvcodec")]
        if let Ok(capability) = sora_sdk::NvCodecVideoCodecCapability::new() {
            capabilities.push(Box::new(capability));
        }
        #[cfg(all(feature = "vpl", target_os = "linux"))]
        if let Ok(capability) = sora_sdk::VplVideoCodecCapability::new() {
            capabilities.push(Box::new(capability));
        }
        #[cfg(feature = "amf")]
        if let Ok(capability) = sora_sdk::AmfVideoCodecCapability::new() {
            capabilities.push(Box::new(capability));
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(capability) = sora_sdk::InternalAppleVideoCodecCapability::new() {
            capabilities.push(Box::new(capability));
        }

        // CLI 指定を preference に反映する
        apply_selection(&mut preference, &capabilities, selection)?;

        info!(
            target: "codec",
            count = preference.codecs().len(),
            "P2P/Ayame video codec factories created"
        );

        let shared: Capabilities = Arc::new(Mutex::new(capabilities));
        let encoder =
            VideoEncoderFactory::new_with_handler(Box::new(SelectedVideoEncoderFactory {
                preference: preference.clone(),
                capabilities: shared.clone(),
            }));
        let decoder =
            VideoDecoderFactory::new_with_handler(Box::new(SelectedVideoDecoderFactory {
                preference,
                capabilities: shared,
            }));
        Ok((encoder, decoder))
    }

    /// CLI 指定を `VideoCodecPreference` に反映する
    fn apply_selection(
        preference: &mut VideoCodecPreference,
        capabilities: &[Box<dyn VideoCodecCapability>],
        selection: &CodecSelection,
    ) -> Result<(), BoxError> {
        for (direction, codec_type, label, value) in [
            (
                CodecDirection::Encoder,
                VideoCodecType::Vp8,
                "VP8 encoder",
                selection.vp8_encoder.as_deref(),
            ),
            (
                CodecDirection::Decoder,
                VideoCodecType::Vp8,
                "VP8 decoder",
                selection.vp8_decoder.as_deref(),
            ),
            (
                CodecDirection::Encoder,
                VideoCodecType::Vp9,
                "VP9 encoder",
                selection.vp9_encoder.as_deref(),
            ),
            (
                CodecDirection::Decoder,
                VideoCodecType::Vp9,
                "VP9 decoder",
                selection.vp9_decoder.as_deref(),
            ),
            (
                CodecDirection::Encoder,
                VideoCodecType::Av1,
                "AV1 encoder",
                selection.av1_encoder.as_deref(),
            ),
            (
                CodecDirection::Decoder,
                VideoCodecType::Av1,
                "AV1 decoder",
                selection.av1_decoder.as_deref(),
            ),
            (
                CodecDirection::Encoder,
                VideoCodecType::H264,
                "H.264 encoder",
                selection.h264_encoder.as_deref(),
            ),
            (
                CodecDirection::Decoder,
                VideoCodecType::H264,
                "H.264 decoder",
                selection.h264_decoder.as_deref(),
            ),
            (
                CodecDirection::Encoder,
                VideoCodecType::H265,
                "H.265 encoder",
                selection.h265_encoder.as_deref(),
            ),
            (
                CodecDirection::Decoder,
                VideoCodecType::H265,
                "H.265 decoder",
                selection.h265_decoder.as_deref(),
            ),
        ] {
            let Some(value) = value else {
                continue;
            };
            let implementation_name = implementation_name(value).ok_or_else(|| -> BoxError {
                format!("unsupported {label} engine: {value}").into()
            })?;
            let capability = capabilities
                .iter()
                .find(|c| c.get_implementation().name() == implementation_name)
                .ok_or_else(|| -> BoxError {
                    format!("{label} engine is not available in this build: {value}").into()
                })?;
            if !capability.is_supported(direction, codec_type) {
                return Err(format!("{label} is not supported by {value}").into());
            }
            let implementation = capability.get_implementation();
            preference
                .get_or_add(direction, codec_type, implementation)
                .set_implementation(capability.get_implementation());
        }
        Ok(())
    }

    /// CLI の値から capability の実装名へ変換する
    fn implementation_name(value: &str) -> Option<&'static str> {
        match value {
            "default" | "software" => Some("internal"),
            "nvidia" => Some("nvcodec"),
            "vpl" => Some("vpl"),
            "amf" => Some("amf"),
            "videotoolbox" => Some("internal-apple"),
            _ => None,
        }
    }

    /// preference の各エントリに対応する capability を探す
    fn find_capability<'a>(
        capabilities: &'a [Box<dyn VideoCodecCapability>],
        implementation: &VideoCodecImplementation,
    ) -> Option<&'a dyn VideoCodecCapability> {
        capabilities
            .iter()
            .find(|c| c.get_implementation() == *implementation)
            .map(|c| c.as_ref())
    }

    /// 指定方向で公開する SDP フォーマット一覧を構築する
    fn collect_supported_formats(
        preference: &VideoCodecPreference,
        capabilities: &[Box<dyn VideoCodecCapability>],
        direction: CodecDirection,
    ) -> Vec<SdpVideoFormat> {
        let mut formats = Vec::new();
        for codec in preference.codecs() {
            if codec.direction() != direction {
                continue;
            }
            let Some(capability) = find_capability(capabilities, codec.implementation()) else {
                continue;
            };
            for format in capability.get_supported_formats(codec.direction()) {
                let format_codec_type = format
                    .name()
                    .ok()
                    .and_then(|name| VideoCodecType::try_from(name.as_str()).ok());
                if format_codec_type != Some(codec.codec_type()) {
                    continue;
                }
                if !formats
                    .iter()
                    .any(|existing: &SdpVideoFormat| existing.is_equal(format.as_ref()))
                {
                    formats.push(format);
                }
            }
        }
        formats
    }

    /// preference に従ってエンコーダーを生成する
    fn create_encoder(
        preference: &VideoCodecPreference,
        capabilities: &[Box<dyn VideoCodecCapability>],
        env: EnvironmentRef<'_>,
        format: SdpVideoFormatRef<'_>,
    ) -> Option<VideoEncoder> {
        let format_name = format.name().ok()?;
        let codec_type = VideoCodecType::try_from(format_name.as_str()).ok()?;
        let preference = preference.find(CodecDirection::Encoder, codec_type)?;
        let capability = find_capability(capabilities, preference.implementation())?;
        let resolved = capability.resolve_sdp_format(CodecDirection::Encoder, format)?;
        capability.create_video_encoder(env, resolved.as_ref())
    }

    /// preference に従ってデコーダーを生成する
    fn create_decoder(
        preference: &VideoCodecPreference,
        capabilities: &[Box<dyn VideoCodecCapability>],
        env: EnvironmentRef<'_>,
        format: SdpVideoFormatRef<'_>,
    ) -> Option<VideoDecoder> {
        let format_name = format.name().ok()?;
        let codec_type = VideoCodecType::try_from(format_name.as_str()).ok()?;
        let preference = preference.find(CodecDirection::Decoder, codec_type)?;
        let capability = find_capability(capabilities, preference.implementation())?;
        let resolved = capability.resolve_sdp_format(CodecDirection::Decoder, format)?;
        capability.create_video_decoder(env, resolved.as_ref())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // software / default 指定でファクトリを構築できることを確認する
        #[test]
        fn build_factories_with_software_selection() {
            let selection = CodecSelection {
                vp8_encoder: Some("software".to_string()),
                vp8_decoder: Some("default".to_string()),
                ..Default::default()
            };
            assert!(build_factories(&selection).is_ok());
        }

        // 未知のバックエンド名はエラーになることを確認する
        #[test]
        fn build_factories_with_unknown_engine() {
            let selection = CodecSelection {
                vp8_encoder: Some("unknown".to_string()),
                ..Default::default()
            };
            assert!(build_factories(&selection).is_err());
        }

        // 利用できないバックエンド名はエラーになることを確認する
        //
        // nvcodec feature が無効なビルドでは capability 自体が存在しない。
        // feature が有効でも VP8 エンコードは非対応のため、いずれの場合もエラーになる。
        #[test]
        fn build_factories_with_unavailable_engine() {
            let selection = CodecSelection {
                vp8_encoder: Some("nvidia".to_string()),
                ..Default::default()
            };
            assert!(build_factories(&selection).is_err());
        }
    }
}

pub(crate) use sora_factory::build_factories;
