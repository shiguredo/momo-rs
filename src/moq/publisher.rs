//! sora-moq の publisher
//!
//! `momo sora-moq publish` の実装。映像 (H.264 / avc1) と音声 (Opus) をキャプチャして
//! エンコードし、MSF カタログとともに MOQT relay へ publish する。
//! payload は 4 バイト長プレフィックス形式、パラメーターセットは Video Config (0x0D) に
//! 載せる (draft-ietf-moq-loc-04 §2.1.3 / §2.1.2)。draft 由来のため将来の改版で
//! 変わる可能性がある。

use std::time::{Duration, Instant};

use bytes::Bytes;
use shiguredo_moqt::error::{
    PUBLISH_DONE_GOING_AWAY, REQUEST_DOES_NOT_EXIST, REQUEST_NOT_SUPPORTED,
};
use shiguredo_moqt::loc::{
    LocProperties, LocProperty, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_TIMESCALE,
    PROP_TIMESTAMP, PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
};
use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::message::common::{Location, TrackNamespace};
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::session::types::{DataStreamId, DataStreamResetReason, RequestStreamEnd};
use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode, SubgroupObject};
use shiguredo_moqt::track_properties::TrackProperties;
use shiguredo_openh264::{EncodeOptions, Encoder, EncoderConfig, FrameType};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::error::BoxError;
use crate::moq::catalog::{self, AudioTrackSpec, VideoTrackSpec};
use crate::moq::client::{ClientEvent, DataPlaneHandle, MoqtClient, ObjectFilterOutcome};
use crate::moq::config::{MoqCommonConfig, MoqConfig};
use crate::moq::error::MoqError;
use crate::moq::h264;
use crate::moq::media::{self, CapturedAudioFrame, CapturedVideoFrame};
use crate::moq::opus::{self, OpusEncoder};
use crate::moq::quic;
use crate::moq::transport::{SendStream, StreamHandle};
use crate::moq::url::ServerUrl;

/// 映像トラックの Track Alias
const VIDEO_TRACK_ALIAS: u64 = 1;
/// 音声トラックの Track Alias
const AUDIO_TRACK_ALIAS: u64 = 2;
/// カタログトラックの Track Alias
const CATALOG_TRACK_ALIAS: u64 = 0;
/// 映像トラックの名前
const VIDEO_TRACK_NAME: &[u8] = b"video";
/// 音声トラックの名前
const AUDIO_TRACK_NAME: &[u8] = b"audio";
/// 映像の Timescale
///
/// draft-ietf-moq-loc-04 §2.3.1.2 は 90000 を例示する。映像は 90000、
/// 音声はサンプリングレート (48000) を使う。
const VIDEO_TIMESCALE: u64 = 90_000;
/// 映像と音声を送る Subgroup の Publisher Priority
const PUBLISHER_PRIORITY: u8 = 128;
/// session の timer を進める間隔
const TICK_INTERVAL: Duration = Duration::from_millis(100);
/// 制御メッセージ / data stream の応答待ちタイムアウト (ms)
const TIMEOUT_MS: u64 = 30_000;
/// 映像キャプチャのチャネル容量 (フレーム数)
const VIDEO_CHANNEL_CAPACITY: usize = 4;
/// 音声キャプチャのチャネル容量 (フレーム数)
const AUDIO_CHANNEL_CAPACITY: usize = 8;

/// エンコード済みの映像フレーム
struct VideoOutput {
    /// 4 バイト長プレフィックス形式の payload
    payload: Vec<u8>,
    /// キーフレームかどうか
    is_keyframe: bool,
    /// LOC の Timestamp
    timestamp: u64,
    /// Video Config (キーフレームのみ)
    video_config: Option<Vec<u8>>,
}

/// キャプチャ時刻を Timescale 単位へ変換する
///
/// 変換の丸めで同じ値が続かないように、直前の値以下になった場合は +1 する。
struct TimestampConverter {
    /// 1 秒あたりの単位数
    timescale: u64,
    /// 直前の値
    last: Option<u64>,
}

impl TimestampConverter {
    /// Timescale を指定して作る
    fn new(timescale: u64) -> Self {
        Self {
            timescale,
            last: None,
        }
    }

    /// キャプチャ時刻 (マイクロ秒) を変換する
    fn convert(&mut self, timestamp_us: i64) -> u64 {
        let converted = if timestamp_us <= 0 {
            0
        } else {
            (timestamp_us as u128 * self.timescale as u128 / 1_000_000) as u64
        };
        let value = match self.last {
            Some(last) if converted <= last => last + 1,
            _ => converted,
        };
        self.last = Some(value);
        value
    }

    /// 直前の値から `delta` 進めた値を返す
    fn advance(&mut self, delta: u64) -> u64 {
        let value = self.last.map_or(0, |last| last + delta);
        self.last = Some(value);
        value
    }
}

/// Subgroup ストリームを 1 本管理するライター
///
/// PUBLISH 済みの subscription (request_id) に対して、Group ごとに 1 本の Subgroup を書く。
struct SubgroupWriter {
    /// Session への通知ハンドル
    data_plane: DataPlaneHandle,
    /// 送信ストリーム
    stream: SendStream,
    /// ストリーム ID
    stream_id: DataStreamId,
    /// Track Alias
    track_alias: u64,
    /// Group ID
    group_id: u64,
    /// Object Properties を持つかどうか (Object の有無と一致させる)
    has_properties: bool,
    /// 次の Object ID
    next_object_id: u64,
    /// 直前の Object ID
    previous_object_id: Option<u64>,
    /// Subgroup Header を wire に書いたかどうか
    header_sent: bool,
    /// フィルタ不通過でスキップした Object があるかどうか
    skipped: bool,
}

impl SubgroupWriter {
    /// Subgroup ストリームを開く
    ///
    /// Subgroup Header は最初の Object を送るときまで wire に書かない
    /// (最初の Object が Object ID 0 かどうかで first_object が変わるため)。
    async fn new(
        handle: &StreamHandle,
        data_plane: &DataPlaneHandle,
        request_id: u64,
        track_alias: u64,
        group_id: u64,
        has_properties: bool,
    ) -> Result<Self, BoxError> {
        let stream = handle.open_send_stream().await?;
        let stream_id = DataStreamId(stream.stream_id());
        let header = SubgroupHeader {
            track_alias,
            group_id,
            subgroup_id: SubgroupIdMode::Zero,
            publisher_priority: Some(PUBLISHER_PRIORITY),
            has_properties,
            end_of_group: false,
            first_object: false,
        };
        data_plane.send_subgroup_header(stream_id, request_id, &header)?;
        Ok(Self {
            data_plane: data_plane.clone(),
            stream,
            stream_id,
            track_alias,
            group_id,
            has_properties,
            next_object_id: 0,
            previous_object_id: None,
            header_sent: false,
            skipped: false,
        })
    }

    /// Object を 1 つ書き込む
    ///
    /// フィルタ不通過の場合は wire へ送らず [`ObjectFilterOutcome::Skip`] を返す。
    async fn write_object(
        &mut self,
        payload: &[u8],
        properties: &LocProperties,
    ) -> Result<ObjectFilterOutcome, BoxError> {
        let object_id = self.next_object_id;
        let object_id_delta = match self.previous_object_id {
            None => object_id,
            Some(previous) => object_id - previous - 1,
        };
        let properties_data =
            if properties.is_empty() {
                None
            } else {
                Some(properties.encode().map_err(|e| {
                    MoqError::Session(format!("failed to encode LOC properties: {e}"))
                })?)
            };
        if self.data_plane.send_subgroup_object(
            self.stream_id,
            object_id,
            properties_data.as_deref(),
        )? == ObjectFilterOutcome::Skip
        {
            self.next_object_id += 1;
            self.skipped = true;
            return Ok(ObjectFilterOutcome::Skip);
        }

        let mut buf = Vec::with_capacity(payload.len() + 64);
        if !self.header_sent {
            let header = SubgroupHeader {
                track_alias: self.track_alias,
                group_id: self.group_id,
                subgroup_id: SubgroupIdMode::Zero,
                publisher_priority: Some(PUBLISHER_PRIORITY),
                has_properties: self.has_properties,
                end_of_group: false,
                first_object: object_id == 0,
            };
            buf.extend_from_slice(&header.encode());
            self.header_sent = true;
        }
        let object = SubgroupObject {
            object_id_delta,
            payload_length: payload.len() as u64,
            status: None,
        };
        object
            .encode(self.has_properties, properties_data.as_deref(), &mut buf)
            .map_err(|e| MoqError::Session(format!("failed to encode subgroup object: {e}")))?;
        buf.extend_from_slice(payload);
        self.stream.send(Bytes::from(buf)).await?;
        self.previous_object_id = Some(object_id);
        self.next_object_id += 1;
        Ok(ObjectFilterOutcome::Pass)
    }

    /// ストリームを終端する
    ///
    /// 1 つも Object を送っていない場合とフィルタでスキップした Object がある場合は、
    /// Group が不完全になるため FIN ではなく RESET で閉じる
    /// (draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams))。
    async fn finish(mut self) -> Result<(), BoxError> {
        if !self.header_sent || self.skipped {
            let error_code = DataStreamResetReason::Cancelled.error_code();
            self.stream.reset(error_code)?;
            self.data_plane.send_data_stream_closed(
                self.stream_id,
                RequestStreamEnd::Reset {
                    error_code,
                    reliable_size: None,
                },
            )?;
        } else {
            self.stream.finish()?;
            self.data_plane
                .send_data_stream_closed(self.stream_id, RequestStreamEnd::Fin)?;
        }
        Ok(())
    }
}

/// カタログの状態
struct CatalogState {
    /// publish 済みかどうか
    ready: bool,
    /// カタログトラックの Request ID
    request_id: Option<u64>,
    /// カタログの JSON
    bytes: Option<Vec<u8>>,
}

/// publisher の実体
struct Publisher {
    /// MOQT クライアント
    client: MoqtClient,
    /// Track Namespace
    namespace: TrackNamespace,
    /// 設定
    config: MoqConfig,
    /// 共通設定
    common: MoqCommonConfig,
    /// ストリーム開設ハンドル
    handle: StreamHandle,
    /// Session 通知ハンドル
    data_plane: DataPlaneHandle,
    /// 映像トラックの Request ID
    video_request_id: Option<u64>,
    /// 音声トラックの Request ID
    audio_request_id: Option<u64>,
    /// 映像エンコーダー
    video_encoder: Option<Encoder>,
    /// 音声エンコーダー
    audio_encoder: Option<OpusEncoder>,
    /// 映像の codec 文字列 (SPS から確定する)
    video_codec: Option<String>,
    /// カタログの状態
    catalog: CatalogState,
    /// カタログ publish 待ちの映像フレーム
    pending_video: Vec<VideoOutput>,
    /// 映像の Subgroup ライター
    video_writer: Option<SubgroupWriter>,
    /// 次の映像 Group ID
    video_group_id: u64,
    /// 映像のタイムスタンプ変換
    video_timestamps: TimestampConverter,
    /// 次の音声 Group ID
    audio_group_id: u64,
    /// 音声のタイムスタンプ変換
    audio_timestamps: TimestampConverter,
    /// Audio Config を送信済みかどうか
    audio_config_sent: bool,
    /// Opus 1 フレームあたりのサンプル数 (チャンネル単位)
    audio_samples_per_frame: usize,
    /// Opus エンコード待ちの PCM
    audio_pcm: Vec<i16>,
}

impl Publisher {
    /// 接続してトラックを publish し、エンコーダーとキャプチャを用意する
    async fn connect(config: MoqConfig, common: MoqCommonConfig) -> Result<Self, BoxError> {
        if config.video {
            if common.openh264_lib.is_none() {
                return Err(MoqError::Config(
                    "publishing video requires '--openh264' to load the OpenH264 library"
                        .to_owned(),
                )
                .into());
            }
            if common.video_width <= 0
                || common.video_height <= 0
                || common.video_width % 2 != 0
                || common.video_height % 2 != 0
            {
                return Err(MoqError::Config(format!(
                    "video size must be a positive even number: {}x{}",
                    common.video_width, common.video_height
                ))
                .into());
            }
        }

        let server = ServerUrl::parse(&config.url)?;
        let connection = quic::connect(&server, common.insecure, common.ca_cert.as_deref()).await?;
        let (mut client, _acceptor) = MoqtClient::establish(connection, &server).await?;
        client.set_control_message_timeout_ms(Some(TIMEOUT_MS));
        client.set_data_stream_timeout_ms(Some(TIMEOUT_MS));

        let namespace = TrackNamespace::new(vec![config.namespace.as_bytes().to_vec()])
            .map_err(|e| MoqError::Session(format!("invalid namespace: {e}")))?;

        let video_request_id = if config.video {
            Some(
                client
                    .publish_track(
                        namespace.clone(),
                        VIDEO_TRACK_NAME.to_vec(),
                        VIDEO_TRACK_ALIAS,
                    )
                    .await?,
            )
        } else {
            None
        };
        let audio_request_id = if config.audio {
            Some(
                client
                    .publish_track(
                        namespace.clone(),
                        AUDIO_TRACK_NAME.to_vec(),
                        AUDIO_TRACK_ALIAS,
                    )
                    .await?,
            )
        } else {
            None
        };

        let video_encoder = if config.video {
            let lib = common
                .openh264_lib
                .clone()
                .ok_or_else(|| MoqError::Config("OpenH264 library is missing".to_owned()))?;
            let mut encoder_config = EncoderConfig::new(
                common.video_width as usize,
                common.video_height as usize,
                (config.video_bit_rate.unwrap_or(0) as usize) * 1000,
                common.framerate as usize,
                1,
            );
            encoder_config.intra_period = Some(config.video_keyframe_interval as usize);
            let encoder = Encoder::new(lib, encoder_config)
                .map_err(|e| MoqError::Media(format!("failed to create H.264 encoder: {e}")))?;
            info!(
                target: "moq",
                width = common.video_width,
                height = common.video_height,
                framerate = common.framerate,
                bit_rate_kbps = config.video_bit_rate.unwrap_or(0),
                keyframe_interval = config.video_keyframe_interval,
                "H.264 encoder created"
            );
            Some(encoder)
        } else {
            None
        };
        let (audio_encoder, audio_samples_per_frame) = if config.audio {
            let encoder = OpusEncoder::new(
                opus::SAMPLE_RATE,
                opus::CHANNELS,
                config.audio_bit_rate.unwrap_or(0) * 1000,
            )?;
            let samples = encoder.samples_per_frame();
            (Some(encoder), samples)
        } else {
            (None, 0)
        };

        let handle = client.handle();
        let data_plane = client.data_plane();
        Ok(Self {
            client,
            namespace,
            config,
            common,
            handle,
            data_plane,
            video_request_id,
            audio_request_id,
            video_encoder,
            audio_encoder,
            video_codec: None,
            catalog: CatalogState {
                ready: false,
                request_id: None,
                bytes: None,
            },
            pending_video: Vec::new(),
            video_writer: None,
            video_group_id: 0,
            video_timestamps: TimestampConverter::new(VIDEO_TIMESCALE),
            audio_group_id: 0,
            audio_timestamps: TimestampConverter::new(opus::SAMPLE_RATE as u64),
            audio_config_sent: false,
            audio_samples_per_frame,
            audio_pcm: Vec::new(),
        })
    }

    /// カタログを publish する
    async fn publish_catalog(&mut self) -> Result<(), BoxError> {
        let video_spec = self.video_codec.as_ref().map(|codec| VideoTrackSpec {
            codec: codec.clone(),
            width: self.common.video_width as u64,
            height: self.common.video_height as u64,
            framerate: self.common.framerate as f64,
            bitrate_kbps: self.config.video_bit_rate.unwrap_or(0),
            timescale: VIDEO_TIMESCALE,
            // maxGopDuration はフレーム数から ms へ換算する (先に除算すると精度が落ちる)
            max_gop_duration_ms: self.config.video_keyframe_interval as u64 * 1000
                / self.common.framerate.max(1) as u64,
        });
        let audio_spec = if self.config.audio {
            Some(AudioTrackSpec {
                codec: "opus".to_owned(),
                samplerate: opus::SAMPLE_RATE as u64,
                channel_config: opus::CHANNELS.to_string(),
                bitrate_kbps: self.config.audio_bit_rate.unwrap_or(0),
            })
        } else {
            None
        };
        let bytes = catalog::encode_catalog(
            &self.config.namespace,
            video_spec.as_ref(),
            audio_spec.as_ref(),
        )?;

        let request_id = self
            .client
            .publish_track(
                self.namespace.clone(),
                catalog::CATALOG_TRACK_NAME.to_vec(),
                CATALOG_TRACK_ALIAS,
            )
            .await?;
        let mut writer = SubgroupWriter::new(
            &self.handle,
            &self.data_plane,
            request_id,
            CATALOG_TRACK_ALIAS,
            0,
            false,
        )
        .await?;
        if writer.write_object(&bytes, &LocProperties::new()).await? == ObjectFilterOutcome::Skip {
            // フィルタでカタログが送られないと購読側がトラックを見つけられない
            return Err(MoqError::Catalog(
                "the catalog object was filtered out by the subscription filter".to_owned(),
            )
            .into());
        }
        writer.finish().await?;
        info!(target: "moq", request_id = request_id, "catalog published");
        self.catalog = CatalogState {
            ready: true,
            request_id: Some(request_id),
            bytes: Some(bytes),
        };
        Ok(())
    }

    /// エンコード済みの映像フレームを処理する
    async fn handle_video_frame(&mut self, frame: CapturedVideoFrame) -> Result<(), BoxError> {
        // キャプチャ設定と違うサイズのフレームはエンコーダーに渡せないため捨てる
        if frame.width != self.common.video_width as usize
            || frame.height != self.common.video_height as usize
        {
            warn!(
                target: "moq",
                width = frame.width,
                height = frame.height,
                expected_width = self.common.video_width,
                expected_height = self.common.video_height,
                "dropping a video frame with an unexpected size"
            );
            return Ok(());
        }
        let encoder = self
            .video_encoder
            .as_mut()
            .ok_or_else(|| MoqError::Media("video encoder is missing".to_owned()))?;
        let options = EncodeOptions { force_idr: false };
        let encoded = encoder
            .encode(&frame.y, &frame.u, &frame.v, &options)
            .map_err(|e| MoqError::Media(format!("failed to encode video frame: {e}")))?;
        let Some(encoded) = encoded else {
            return Ok(());
        };
        let is_keyframe = matches!(encoded.frame_type, FrameType::Idr);
        let video_config = if is_keyframe {
            if encoded.sps_list.is_empty() || encoded.pps_list.is_empty() {
                return Err(MoqError::Media(
                    "encoded keyframe does not contain SPS/PPS".to_owned(),
                )
                .into());
            }
            let codec = h264::avc_codec_string_from_sps(&encoded.sps_list[0])?;
            self.video_codec = Some(codec);
            Some(h264::build_avc_decoder_config_record(
                &encoded.sps_list,
                &encoded.pps_list,
            )?)
        } else {
            None
        };
        let payload = h264::annex_b_to_length_prefixed(&encoded.data)?;
        let timestamp = self.video_timestamps.convert(frame.timestamp_us);
        let output = VideoOutput {
            payload,
            is_keyframe,
            timestamp,
            video_config,
        };

        if !self.catalog.ready {
            // 最初のキーフレームから SPS/PPS と codec 文字列が確定するまでカタログを publish できない
            self.pending_video.push(output);
            if self.video_codec.is_some() {
                self.publish_catalog().await?;
                let pending = std::mem::take(&mut self.pending_video);
                for output in pending {
                    self.send_video_object(output).await?;
                }
            }
            return Ok(());
        }
        self.send_video_object(output).await
    }

    /// 映像 Object を送る
    async fn send_video_object(&mut self, output: VideoOutput) -> Result<(), BoxError> {
        if output.is_keyframe {
            if let Some(writer) = self.video_writer.take() {
                writer.finish().await?;
            }
            let request_id = self
                .video_request_id
                .ok_or_else(|| MoqError::Session("video track is not published".to_owned()))?;
            let writer = SubgroupWriter::new(
                &self.handle,
                &self.data_plane,
                request_id,
                VIDEO_TRACK_ALIAS,
                self.video_group_id,
                true,
            )
            .await?;
            self.video_group_id += 1;
            self.video_writer = Some(writer);
        }
        let Some(writer) = self.video_writer.as_mut() else {
            // キーフレームより前のフレームは Group が無いため送らない
            return Ok(());
        };

        let mut properties = LocProperties::new();
        // フレーム種別は Video Frame Marking (draft-ietf-moq-loc-04 §2.3.2.2 / RFC 9626) で通知する。
        // 1 object = 1 frame のため S と E は常に 1、I はキーフレームのときだけ 1
        // (I ビット = 0x20)。LOC の奇数 ID はバイト列で載せる。
        let frame_marking: u8 = if output.is_keyframe { 0xE0 } else { 0xC0 };
        properties.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![frame_marking]),
        });
        properties.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(output.timestamp),
        });
        properties.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(VIDEO_TIMESCALE),
        });
        if let Some(video_config) = output.video_config {
            properties.push(LocProperty {
                prop_id: PROP_VIDEO_CONFIG,
                value: LocPropertyValue::Bytes(video_config),
            });
        }
        // フィルタ不通過のフレームは送られない (Skip は正常な動作)
        let _ = writer.write_object(&output.payload, &properties).await?;
        Ok(())
    }

    /// キャプチャした音声フレームを処理する
    async fn handle_audio_frame(&mut self, frame: CapturedAudioFrame) -> Result<(), BoxError> {
        if frame.sample_rate != opus::SAMPLE_RATE {
            warn!(
                target: "moq",
                sample_rate = frame.sample_rate,
                "dropping audio frame with an unsupported sample rate"
            );
            return Ok(());
        }
        // ステレオへ揃える (モノラルは複製する)
        let pcm: Vec<i16> = match frame.channels {
            1 => frame
                .pcm
                .iter()
                .flat_map(|sample| [*sample, *sample])
                .collect(),
            2 => frame.pcm,
            other => {
                warn!(target: "moq", channels = other, "dropping audio frame with an unsupported channel count");
                return Ok(());
            }
        };
        if self.audio_timestamps.last.is_none() {
            // 最初のフレームのキャプチャ時刻から Timestamp を始める
            self.audio_timestamps.convert(frame.timestamp_us);
        }
        self.audio_pcm.extend_from_slice(&pcm);

        let channels = opus::CHANNELS as usize;
        let frame_samples = self.audio_samples_per_frame * channels;
        while self.audio_pcm.len() >= frame_samples {
            let chunk: Vec<i16> = self.audio_pcm.drain(..frame_samples).collect();
            let encoder = self
                .audio_encoder
                .as_mut()
                .ok_or_else(|| MoqError::Media("audio encoder is missing".to_owned()))?;
            let packet = encoder.encode(&chunk)?;
            let timestamp = self
                .audio_timestamps
                .advance(self.audio_samples_per_frame as u64);
            self.send_audio_object(&packet, timestamp).await?;
        }
        Ok(())
    }

    /// 音声 Object を送る
    ///
    /// draft-ietf-moq-loc-04 §4.1 の例に合わせ、1 Opus パケットを 1 Object / 1 Group で送る。
    async fn send_audio_object(&mut self, packet: &[u8], timestamp: u64) -> Result<(), BoxError> {
        let request_id = self
            .audio_request_id
            .ok_or_else(|| MoqError::Session("audio track is not published".to_owned()))?;
        let mut writer = SubgroupWriter::new(
            &self.handle,
            &self.data_plane,
            request_id,
            AUDIO_TRACK_ALIAS,
            self.audio_group_id,
            true,
        )
        .await?;
        self.audio_group_id += 1;

        let mut properties = LocProperties::new();
        properties.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(timestamp),
        });
        properties.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(opus::SAMPLE_RATE as u64),
        });
        if !self.audio_config_sent {
            properties.push(LocProperty {
                prop_id: PROP_AUDIO_CONFIG,
                value: LocPropertyValue::Bytes(opus::build_opus_head(
                    opus::SAMPLE_RATE,
                    opus::CHANNELS,
                )),
            });
        }
        // フィルタ不通過でスキップされた場合は Audio Config を次の Object に載せ直す
        if writer.write_object(packet, &properties).await? == ObjectFilterOutcome::Pass
            && !self.audio_config_sent
        {
            self.audio_config_sent = true;
        }
        writer.finish().await
    }

    /// イベントを処理する。true を返した場合は終了する
    async fn handle_event(&mut self, event: ClientEvent) -> Result<bool, BoxError> {
        match event {
            ClientEvent::Session(event) => match event {
                shiguredo_moqt::session::types::SessionEvent::CloseSession(error) => {
                    warn!(target: "moq", code = error.code, "session closed by peer");
                    Ok(true)
                }
                shiguredo_moqt::session::types::SessionEvent::GoawayReceived { .. } => {
                    info!(target: "moq", "received GOAWAY");
                    Ok(true)
                }
                shiguredo_moqt::session::types::SessionEvent::PublishDoneReceived {
                    request_id,
                    status_code,
                    ..
                } => {
                    info!(
                        target: "moq",
                        request_id = request_id,
                        status_code = status_code,
                        "received PUBLISH_DONE"
                    );
                    Ok(true)
                }
                other => {
                    tracing::debug!(target: "moq", event = ?other, "session event");
                    Ok(false)
                }
            },
            ClientEvent::Request(request) => {
                self.serve_request(request).await?;
                Ok(false)
            }
            ClientEvent::RequestUpdate(update) => {
                self.client.send_request_ok(update.request_id).await?;
                Ok(false)
            }
        }
    }

    /// peer (relay) からの要求に応答する
    async fn serve_request(
        &mut self,
        request: crate::moq::client::IncomingRequest,
    ) -> Result<(), BoxError> {
        match request.message {
            ControlMessage::Subscribe(subscribe) => {
                let alias = if subscribe.track_name == VIDEO_TRACK_NAME
                    && self.video_request_id.is_some()
                {
                    Some(VIDEO_TRACK_ALIAS)
                } else if subscribe.track_name == AUDIO_TRACK_NAME
                    && self.audio_request_id.is_some()
                {
                    Some(AUDIO_TRACK_ALIAS)
                } else {
                    None
                };
                match alias {
                    Some(track_alias) => {
                        self.client
                            .send_subscribe_ok(
                                request.request_id,
                                track_alias,
                                MessageParameters::new(),
                                TrackProperties::new(),
                            )
                            .await?;
                        info!(
                            target: "moq",
                            request_id = request.request_id,
                            track = %String::from_utf8_lossy(&subscribe.track_name),
                            "accepted SUBSCRIBE"
                        );
                    }
                    None => {
                        self.client
                            .send_request_error(
                                request.request_id,
                                REQUEST_DOES_NOT_EXIST,
                                "unknown track",
                            )
                            .await?;
                    }
                }
            }
            ControlMessage::Fetch(fetch) => {
                if fetch.track_name == catalog::CATALOG_TRACK_NAME {
                    let Some(bytes) = self.catalog.bytes.clone() else {
                        self.client
                            .send_request_error(
                                request.request_id,
                                REQUEST_DOES_NOT_EXIST,
                                "catalog is not ready",
                            )
                            .await?;
                        return Ok(());
                    };
                    self.client
                        .send_fetch_ok(
                            request.request_id,
                            0,
                            Location {
                                group_id: 0,
                                object_id: 0,
                            },
                            MessageParameters::new(),
                            TrackProperties::new(),
                        )
                        .await?;
                    self.client
                        .send_fetch_response(request.request_id, 0, 0, &bytes, None)
                        .await?;
                    info!(target: "moq", request_id = request.request_id, "served catalog FETCH");
                } else {
                    self.client
                        .send_request_error(
                            request.request_id,
                            REQUEST_DOES_NOT_EXIST,
                            "unknown track",
                        )
                        .await?;
                }
            }
            other => {
                warn!(target: "moq", message = ?other, "rejecting unsupported peer request");
                self.client
                    .send_request_error(
                        request.request_id,
                        REQUEST_NOT_SUPPORTED,
                        "unsupported request",
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// 終了処理を行う
    async fn finish(&mut self) {
        if let Some(writer) = self.video_writer.take()
            && let Err(e) = writer.finish().await
        {
            warn!(target: "moq", "failed to finish the video subgroup: {e}");
        }
        for request_id in [
            self.catalog.request_id,
            self.video_request_id,
            self.audio_request_id,
        ]
        .into_iter()
        .flatten()
        {
            if let Err(e) = self
                .client
                .send_publish_done(request_id, PUBLISH_DONE_GOING_AWAY, "")
                .await
            {
                warn!(target: "moq", request_id = request_id, "failed to send PUBLISH_DONE: {e}");
            }
        }
        if let Err(e) = self.client.send_goaway(Vec::new(), 5000).await {
            warn!(target: "moq", "failed to send GOAWAY: {e}");
        }
        if let Err(e) = self.client.close(0, "").await {
            warn!(target: "moq", "failed to close the session: {e}");
        }
    }
}

/// `momo sora-moq publish` を実行する
pub async fn run(config: MoqConfig, common: MoqCommonConfig) -> Result<(), BoxError> {
    let mut publisher = Publisher::connect(config, common).await?;

    let (video_tx, mut video_rx) = mpsc::channel::<CapturedVideoFrame>(VIDEO_CHANNEL_CAPACITY);
    let (audio_tx, mut audio_rx) = mpsc::channel::<CapturedAudioFrame>(AUDIO_CHANNEL_CAPACITY);
    let _video_guard = if publisher.config.video {
        Some(media::start_video_capture(
            &publisher.common.clone(),
            video_tx,
        )?)
    } else {
        None
    };
    let _audio_guard = if publisher.config.audio {
        Some(media::start_audio_capture(
            &publisher.common.clone(),
            audio_tx,
        )?)
    } else {
        None
    };

    // 映像が無い場合は最初からカタログを作れる
    if !publisher.config.video {
        publisher.publish_catalog().await?;
    }

    let mut tick = tokio::time::interval(TICK_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let start = Instant::now();
    let mut stopping = false;
    while !stopping {
        tokio::select! {
            Some(frame) = video_rx.recv(), if publisher.config.video => {
                publisher.handle_video_frame(frame).await?;
            }
            Some(frame) = audio_rx.recv(), if publisher.config.audio => {
                publisher.handle_audio_frame(frame).await?;
            }
            event = publisher.client.next_event() => {
                if let Some(event) = event? {
                    stopping = publisher.handle_event(event).await?;
                }
            }
            _ = tick.tick() => {
                publisher.client.tick(start.elapsed().as_millis() as u64);
            }
        }
    }

    publisher.finish().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// タイムスタンプ変換が丸めで重複しないこと
    #[test]
    fn test_timestamp_converter_keeps_increasing() {
        let mut converter = TimestampConverter::new(VIDEO_TIMESCALE);
        let first = converter.convert(1_000_000);
        assert_eq!(first, VIDEO_TIMESCALE);
        // 丸めで同じ値になる入力でも +1 される
        let second = converter.convert(1_000_001);
        assert_eq!(second, first + 1);
        let third = converter.convert(2_000_000);
        assert_eq!(third, VIDEO_TIMESCALE * 2);
    }

    /// 負のタイムスタンプを 0 として扱うこと
    #[test]
    fn test_timestamp_converter_handles_negative() {
        let mut converter = TimestampConverter::new(48_000);
        assert_eq!(converter.convert(-1), 0);
    }

    /// advance が直前の値から進めること
    #[test]
    fn test_timestamp_converter_advance() {
        let mut converter = TimestampConverter::new(48_000);
        converter.convert(0);
        assert_eq!(converter.advance(960), 960);
        assert_eq!(converter.advance(960), 1920);
    }
}
