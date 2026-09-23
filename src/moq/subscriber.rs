//! sora-moq の subscriber
//!
//! `momo sora-moq subscribe` の実装。カタログを FETCH して映像と音声のトラックを見つけ、
//! SUBSCRIBE して受信した Object を復号し、raw_player で表示・再生する。
//! 映像は H.264 (avc1、4 バイト長プレフィックス、パラメーターセットは Video Config)、
//! 音声は Opus (Audio Config に OpusHead) を前提とする
//! (draft-ietf-moq-loc-04 §2.1.3 / §2.3.1 / §2.3.2)。draft 由来のため将来の改版で
//! 変わる可能性がある。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::error::REQUEST_NOT_SUPPORTED;
use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::message::common::{Location, TrackNamespace};
use shiguredo_moqt::message_parameter::{
    LocationFilter, MessageParameter, MessageParameterValue, MessageParameters,
    PARAM_LOCATION_FILTER,
};
use shiguredo_moqt::msf::MsfCatalog;
use shiguredo_moqt::session::types::{DataStreamId, SessionEvent, TrackDataAcceptance};
use shiguredo_moqt::stream::decoder::{
    DecodedSubgroupObject, FetchStreamDecoder, SubgroupStreamDecoder,
};
use shiguredo_moqt::stream::{DataStreamType, classify_data_stream_type};
use shiguredo_openh264::{Decoder as H264Decoder, Openh264Library};
use tokio::sync::{Mutex, Semaphore};
use tracing::{info, warn};

use crate::error::BoxError;
use crate::moq::catalog;
use crate::moq::client::{ClientEvent, DataPlaneHandle, MoqtClient};
use crate::moq::config::{MoqCommonConfig, MoqConfig};
use crate::moq::error::MoqError;
use crate::moq::h264;
use crate::moq::opus::{self, OpusDecoder};
use crate::moq::quic;
use crate::moq::transport::{RecvChunk, RecvStream, StreamAcceptor};
use crate::moq::url::ServerUrl;

/// session の timer を進める間隔
const TICK_INTERVAL: Duration = Duration::from_millis(100);
/// 制御メッセージ / data stream の応答待ちタイムアウト (ms)
const TIMEOUT_MS: u64 = 30_000;
/// 同時に処理する data stream の本数
///
/// 映像は 1 group = 1 stream、音声は 1 object = 1 stream で届く。
const MAX_CONCURRENT_STREAMS: usize = 4;
/// 表示待ちの映像フレーム数の上限
///
/// これを超えたらグループまるごと捨てて受信を優先する。途中のフレームだけを捨てると
/// 参照フレームを失って復号できなくなるため、group 単位で捨てる。
const MAX_DISPLAY_BACKLOG: i64 = 20;
/// 映像のフレーム間隔の初期値 (30 fps 相当、マイクロ秒)
const DEFAULT_VIDEO_FRAME_INTERVAL_US: i64 = 33_333;

/// 復号済みの映像フレーム (I420)
pub struct DecodedVideoFrame {
    /// Y プレーン
    pub y: Vec<u8>,
    /// U プレーン
    pub u: Vec<u8>,
    /// V プレーン
    pub v: Vec<u8>,
    /// 幅 (px)
    pub width: i32,
    /// 高さ (px)
    pub height: i32,
    /// 表示時刻 (マイクロ秒)
    pub pts_us: i64,
}

/// 復号済みの音声フレーム (S16 インターリーブ)
pub struct DecodedAudioFrame {
    /// PCM
    pub pcm: Vec<i16>,
    /// サンプリングレート (Hz)
    pub sample_rate: u32,
    /// チャンネル数
    pub channels: u8,
    /// 再生時刻 (マイクロ秒)
    pub pts_us: i64,
}

/// トラックの種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    /// 映像
    Video,
    /// 音声
    Audio,
}

/// data stream を処理するタスクが使う共有コンテキスト
#[derive(Clone)]
struct StreamContext {
    /// Session 通知ハンドル
    data_plane: DataPlaneHandle,
    /// Track Alias → 種別
    track_map: Arc<HashMap<u64, TrackKind>>,
    /// OpenH264 ライブラリ (映像がある場合のみ)
    openh264_lib: Option<Openh264Library>,
    /// Opus デコーダ (音声がある場合のみ、購読全体で 1 個を共有する)
    opus_decoder: Option<Arc<Mutex<OpusDecoder>>>,
    /// 復号済み映像フレームの送信先
    video_tx: std::sync::mpsc::Sender<DecodedVideoFrame>,
    /// 復号済み音声フレームの送信先
    audio_tx: std::sync::mpsc::Sender<DecodedAudioFrame>,
    /// 表示待ちの映像フレーム数
    backlog: Arc<AtomicI64>,
}

/// stream から読み出したデータまたは終端
enum StreamData {
    /// 受信データ
    Data(bytes::Bytes),
    /// 終端
    End,
}

/// `momo sora-moq subscribe` を実行する
///
/// 復号した映像と音声は `video_tx` / `audio_tx` へ送る。呼び出し側は
/// [`run_player`] で再生する。
pub async fn run(
    config: MoqConfig,
    common: MoqCommonConfig,
    video_tx: std::sync::mpsc::Sender<DecodedVideoFrame>,
    audio_tx: std::sync::mpsc::Sender<DecodedAudioFrame>,
    backlog: Arc<AtomicI64>,
) -> Result<(), BoxError> {
    let server = ServerUrl::parse(&config.url)?;
    let connection = quic::connect(&server, common.insecure, common.ca_cert.as_deref()).await?;
    let (mut client, mut acceptor) = MoqtClient::establish(connection, &server).await?;
    client.set_control_message_timeout_ms(Some(TIMEOUT_MS));
    client.set_data_stream_timeout_ms(Some(TIMEOUT_MS));

    let namespace = TrackNamespace::new(vec![config.namespace.as_bytes().to_vec()])
        .map_err(|e| MoqError::Session(format!("invalid namespace: {e}")))?;
    let data_plane = client.data_plane();

    // カタログを FETCH する。Full カタログ (object 0) と delta 更新 (object 1) を要求する
    let mut parameters = MessageParameters::new();
    parameters.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 0,
                    object_id: 0,
                },
                end_group_delta: 0,
                end_object: 1,
            }
            .encode_to_bytes(),
        ),
    });
    let fetch_result = client
        .fetch(
            namespace.clone(),
            catalog::CATALOG_TRACK_NAME.to_vec(),
            parameters,
        )
        .await?;
    let msf_catalog = receive_catalog(&mut acceptor, &data_plane, fetch_result.request_id).await?;

    // カタログから映像と音声のトラックを見つける (識別は codec の接頭辞で行う)
    let video_info = match catalog::find_video_track(&msf_catalog) {
        Ok(info) => Some(info),
        Err(e) => {
            info!(target: "moq", "catalog has no usable video track: {e}");
            None
        }
    };
    let audio_info = match catalog::find_audio_track(&msf_catalog) {
        Ok(info) => Some(info),
        Err(e) => {
            info!(target: "moq", "catalog has no usable audio track: {e}");
            None
        }
    };
    if video_info.is_none() && audio_info.is_none() {
        return Err(MoqError::Catalog("catalog has no usable track".to_owned()).into());
    }
    if let Some(ref video) = video_info {
        if !(video.codec.starts_with("avc1") || video.codec.starts_with("avc3")) {
            return Err(
                MoqError::Media(format!("unsupported video codec: {}", video.codec)).into(),
            );
        }
        if common.openh264_lib.is_none() {
            return Err(MoqError::Config(
                "subscribing video requires '--openh264' to load the OpenH264 library".to_owned(),
            )
            .into());
        }
    }

    // 見つかったトラックを SUBSCRIBE する
    let mut track_map = HashMap::new();
    let mut video_request_id = None;
    if let Some(ref video) = video_info {
        let result = client
            .subscribe_track(namespace.clone(), video.track_name.clone())
            .await?;
        info!(
            target: "moq",
            track = %String::from_utf8_lossy(&video.track_name),
            codec = %video.codec,
            size = format!("{}x{}", video.width, video.height),
            framerate = video.framerate,
            "subscribed to video track"
        );
        track_map.insert(result.track_alias, TrackKind::Video);
        video_request_id = Some(result.request_id);
    }
    let mut audio_request_id = None;
    let mut opus_decoder = None;
    if let Some(ref audio) = audio_info {
        let result = client
            .subscribe_track(namespace.clone(), audio.track_name.clone())
            .await?;
        info!(
            target: "moq",
            track = %String::from_utf8_lossy(&audio.track_name),
            codec = %audio.codec,
            samplerate = audio.samplerate,
            channels = audio.channels,
            "subscribed to audio track"
        );
        track_map.insert(result.track_alias, TrackKind::Audio);
        audio_request_id = Some(result.request_id);
        if audio.samplerate != opus::SAMPLE_RATE {
            warn!(
                target: "moq",
                samplerate = audio.samplerate,
                "unexpected Opus sample rate"
            );
        }
        opus_decoder = Some(Arc::new(Mutex::new(OpusDecoder::new(
            audio.samplerate,
            audio.channels,
        )?)));
    }

    let context = StreamContext {
        data_plane: data_plane.clone(),
        track_map: Arc::new(track_map),
        openh264_lib: common.openh264_lib.clone(),
        opus_decoder,
        video_tx,
        audio_tx,
        backlog,
    };
    let acceptor_task = tokio::spawn(accept_streams(acceptor, context));

    // session のイベントとタイマーを回す
    let mut tick = tokio::time::interval(TICK_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let start = Instant::now();
    let mut stopping = false;
    while !stopping {
        tokio::select! {
            event = client.next_event() => {
                if let Some(event) = event? {
                    stopping = handle_event(&mut client, event).await?;
                }
            }
            _ = tick.tick() => {
                client.tick(start.elapsed().as_millis() as u64);
            }
        }
    }

    // 終了処理: 購読を停止し、GOAWAY を送ってから接続を閉じる
    for request_id in [video_request_id, audio_request_id].into_iter().flatten() {
        if let Err(e) = client.stop_sending(request_id).await {
            warn!(target: "moq", request_id = request_id, "failed to stop the subscription: {e}");
        }
    }
    if let Err(e) = client.send_goaway(Vec::new(), 5000).await {
        warn!(target: "moq", "failed to send GOAWAY: {e}");
    }
    if let Err(e) = client.close(0, "").await {
        warn!(target: "moq", "failed to close the session: {e}");
    }
    acceptor_task.abort();
    Ok(())
}

/// session のイベントを処理する。true を返した場合は終了する
async fn handle_event(client: &mut MoqtClient, event: ClientEvent) -> Result<bool, BoxError> {
    match event {
        ClientEvent::Session(event) => match event {
            SessionEvent::CloseSession(error) => {
                warn!(target: "moq", code = error.code, "session closed by peer");
                Ok(true)
            }
            SessionEvent::GoawayReceived { .. } => {
                info!(target: "moq", "received GOAWAY");
                Ok(true)
            }
            SessionEvent::PublishDoneReceived {
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
        ClientEvent::RequestUpdate(update) => {
            client.send_request_ok(update.request_id).await?;
            Ok(false)
        }
        ClientEvent::Request(request) => {
            warn!(target: "moq", request_id = request.request_id, "rejecting peer request");
            client
                .send_request_error(
                    request.request_id,
                    REQUEST_NOT_SUPPORTED,
                    "subscriber does not serve requests",
                )
                .await?;
            Ok(false)
        }
    }
}

/// カタログ FETCH の応答ストリームを受信してカタログを組み立てる
async fn receive_catalog(
    acceptor: &mut StreamAcceptor,
    data_plane: &DataPlaneHandle,
    expected_request_id: u64,
) -> Result<MsfCatalog, BoxError> {
    loop {
        let mut stream = acceptor.accept_recv_stream().await?.ok_or_else(|| {
            MoqError::Session("connection closed before catalog response".to_owned())
        })?;
        let stream_id = DataStreamId(stream.stream_id());
        let (stream_type_id, buf) = peek_stream_type(&mut stream).await?;
        data_plane.recv_data_stream_type(stream_id, stream_type_id)?;
        match classify_data_stream_type(stream_type_id) {
            Some(DataStreamType::Fetch) => {
                return decode_catalog_stream(
                    stream,
                    buf,
                    data_plane,
                    stream_id,
                    expected_request_id,
                )
                .await;
            }
            Some(DataStreamType::Padding) => {
                drain_stream(&mut stream, data_plane, stream_id).await?;
            }
            other => {
                drain_stream(&mut stream, data_plane, stream_id).await?;
                return Err(MoqError::Session(format!(
                    "unexpected data stream while waiting for the catalog: {other:?}"
                ))
                .into());
            }
        }
    }
}

/// カタログの FETCH 応答ストリームをデコードする
async fn decode_catalog_stream(
    mut stream: RecvStream,
    buf: Vec<u8>,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
    expected_request_id: u64,
) -> Result<MsfCatalog, BoxError> {
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&buf);

    let header = loop {
        match decoder
            .try_decode_header()
            .map_err(|e| MoqError::Session(format!("failed to decode fetch header: {e}")))?
        {
            Some(header) => break header,
            None => match receive_stream_data(&mut stream, data_plane, stream_id).await? {
                StreamData::Data(data) => decoder.push(&data),
                StreamData::End => {
                    return Err(MoqError::Session(
                        "catalog fetch stream ended before the header".to_owned(),
                    )
                    .into());
                }
            },
        }
    };
    if header.request_id != expected_request_id {
        drain_stream(&mut stream, data_plane, stream_id).await?;
        return Err(MoqError::Session(format!(
            "unexpected fetch request id: {} (expected {expected_request_id})",
            header.request_id
        ))
        .into());
    }
    data_plane.recv_fetch_header(stream_id, &header)?;

    let mut catalog: Option<MsfCatalog> = None;
    loop {
        let entry = loop {
            match decoder
                .try_decode_entry()
                .map_err(|e| MoqError::Catalog(format!("failed to decode fetch entry: {e}")))?
            {
                Some(entry) => break entry,
                None => match receive_stream_data(&mut stream, data_plane, stream_id).await? {
                    StreamData::Data(data) => decoder.push(&data),
                    // 終端は receive_stream_data が recv_data_stream_closed で Session に通知済み
                    StreamData::End => {
                        return catalog.ok_or_else(|| {
                            MoqError::Catalog("catalog fetch stream is empty".to_owned()).into()
                        });
                    }
                },
            }
        };
        data_plane.recv_fetch_entry(stream_id)?;
        let payload_length = match &entry {
            shiguredo_moqt::stream::decoder::DecodedFetchEntry::Object(object) => {
                object.payload_length
            }
            _ => 0,
        };
        if payload_length == 0 {
            continue;
        }
        let payload = loop {
            if let Some(payload) = decoder.try_read_payload() {
                break payload;
            }
            match receive_stream_data(&mut stream, data_plane, stream_id).await? {
                StreamData::Data(data) => decoder.push(&data),
                StreamData::End => {
                    return Err(MoqError::Catalog(
                        "catalog fetch stream ended in the middle of an object".to_owned(),
                    )
                    .into());
                }
            }
        };
        let document = catalog::decode_catalog(&payload)?;
        match document {
            shiguredo_moqt::msf::MsfCatalogDocument::Full(full) => {
                catalog = Some(full);
            }
            shiguredo_moqt::msf::MsfCatalogDocument::Delta(delta) => {
                let base = catalog.as_mut().ok_or_else(|| {
                    MoqError::Catalog("received a delta catalog before the full catalog".to_owned())
                })?;
                base.apply_delta(&delta, None).map_err(|e| {
                    MoqError::Catalog(format!("failed to apply delta catalog: {e}"))
                })?;
            }
        }
    }
}

/// 単方向ストリームを受け入れて処理する
async fn accept_streams(mut acceptor: StreamAcceptor, context: StreamContext) {
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_STREAMS));
    loop {
        let stream = match acceptor.accept_recv_stream().await {
            Ok(Some(stream)) => stream,
            Ok(None) => break,
            Err(e) => {
                warn!(target: "moq", "failed to accept a data stream: {e}");
                break;
            }
        };
        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            break;
        };
        let context = context.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_stream(stream, context).await {
                warn!(target: "moq", "failed to handle a data stream: {e}");
            }
        });
    }
    info!(target: "moq", "data stream acceptor stopped");
}

/// 単方向ストリームを 1 本処理する
async fn handle_stream(mut stream: RecvStream, context: StreamContext) -> Result<(), BoxError> {
    let stream_id = DataStreamId(stream.stream_id());
    let (stream_type_id, buf) = peek_stream_type(&mut stream).await?;
    context
        .data_plane
        .recv_data_stream_type(stream_id, stream_type_id)?;
    match classify_data_stream_type(stream_type_id) {
        Some(DataStreamType::Subgroup) => {
            handle_subgroup_stream(stream, buf, stream_id, context).await
        }
        Some(DataStreamType::Fetch) => {
            // カタログ以外の FETCH は要求していないため読み捨てる
            drain_stream(&mut stream, &context.data_plane, stream_id).await
        }
        Some(DataStreamType::Padding) => {
            drain_stream(&mut stream, &context.data_plane, stream_id).await
        }
        None => {
            Err(MoqError::Session(format!("unexpected data stream type: {stream_type_id}")).into())
        }
    }
}

/// Subgroup ストリームを処理する
async fn handle_subgroup_stream(
    mut stream: RecvStream,
    buf: Vec<u8>,
    stream_id: DataStreamId,
    context: StreamContext,
) -> Result<(), BoxError> {
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&buf);
    let header = loop {
        match decoder
            .try_decode_header()
            .map_err(|e| MoqError::Session(format!("failed to decode subgroup header: {e}")))?
        {
            Some(header) => break header,
            None => match receive_stream_data(&mut stream, &context.data_plane, stream_id).await? {
                StreamData::Data(data) => decoder.push(&data),
                StreamData::End => return Ok(()),
            },
        }
    };
    let acceptance = context
        .data_plane
        .recv_subgroup_header(stream_id, &header)?;
    if !matches!(acceptance, TrackDataAcceptance::Accepted) {
        info!(target: "moq", track_alias = header.track_alias, acceptance = ?acceptance, "dropping subgroup stream");
        let _ = context.data_plane.send_data_stream_stop_sending(stream_id);
        return drain_stream(&mut stream, &context.data_plane, stream_id).await;
    }

    match context.track_map.get(&header.track_alias) {
        Some(TrackKind::Video) => {
            // 表示が追いついていない場合はグループまるごと捨てる
            if context.backlog.load(Ordering::Relaxed) > MAX_DISPLAY_BACKLOG {
                let _ = context.data_plane.send_data_stream_stop_sending(stream_id);
                return drain_stream(&mut stream, &context.data_plane, stream_id).await;
            }
            decode_video_stream(stream, decoder, stream_id, context).await
        }
        Some(TrackKind::Audio) => decode_audio_stream(stream, decoder, stream_id, context).await,
        None => {
            info!(target: "moq", track_alias = header.track_alias, "dropping unknown track stream");
            let _ = context.data_plane.send_data_stream_stop_sending(stream_id);
            drain_stream(&mut stream, &context.data_plane, stream_id).await
        }
    }
}

/// 映像 Subgroup を復号する
async fn decode_video_stream(
    mut stream: RecvStream,
    mut decoder: SubgroupStreamDecoder,
    stream_id: DataStreamId,
    context: StreamContext,
) -> Result<(), BoxError> {
    let mut h264_decoder: Option<H264Decoder> = None;
    let mut parameter_sets: Option<Vec<Vec<u8>>> = None;
    let mut last_pts_us = 0i64;
    loop {
        let Some(object) =
            next_subgroup_object(&mut stream, &mut decoder, &context.data_plane, stream_id).await?
        else {
            return Ok(());
        };
        let acceptance = context
            .data_plane
            .recv_subgroup_object(stream_id, &object)?;
        let Some(payload) =
            read_subgroup_payload(&mut stream, &mut decoder, &context.data_plane, stream_id)
                .await?
        else {
            return Ok(());
        };
        if !matches!(acceptance, TrackDataAcceptance::Accepted) {
            continue;
        }
        if object.status.is_some() || payload.is_empty() {
            continue;
        }

        let properties = parse_loc_properties(object.properties_bytes.as_deref())?;
        let pts_us = match compute_pts_us(properties.as_ref()) {
            Some(pts) => {
                last_pts_us = pts;
                pts
            }
            None => last_pts_us + DEFAULT_VIDEO_FRAME_INTERVAL_US,
        };
        if let Some(properties) = properties.as_ref()
            && let Some(config) = properties.video_config()
        {
            let config = h264::parse_avc_decoder_config_record(config)?;
            parameter_sets = Some(
                config
                    .sps
                    .iter()
                    .chain(config.pps.iter())
                    .cloned()
                    .collect(),
            );
        }
        let is_keyframe = properties
            .as_ref()
            .and_then(|properties| properties.video_frame_marking())
            .is_some_and(|marking| marking.first().is_some_and(|byte| byte & 0x20 != 0));
        // Video Config が届く前の Object は復号できないため読み飛ばす
        let Some(parameter_sets) = parameter_sets.as_ref() else {
            continue;
        };
        let Some(lib) = context.openh264_lib.as_ref() else {
            continue;
        };
        let mut annex_b = h264::length_prefixed_to_annex_b(&payload, h264::LENGTH_SIZE)?;
        if is_keyframe {
            // キーフレームでは Video Config の SPS/PPS を payload の前へ付ける
            let mut with_parameter_sets = h264::nals_to_annex_b(parameter_sets);
            with_parameter_sets.extend_from_slice(&annex_b);
            annex_b = with_parameter_sets;
        }
        let decoder = match h264_decoder {
            Some(ref mut decoder) => decoder,
            None => h264_decoder
                .insert(H264Decoder::new(lib.clone()).map_err(|e| {
                    MoqError::Media(format!("failed to create H.264 decoder: {e}"))
                })?),
        };
        match decoder.decode(&annex_b) {
            Ok(Some(frame)) => {
                let width = frame.width();
                let height = frame.height();
                let decoded = DecodedVideoFrame {
                    y: copy_plane(frame.y_plane(), frame.y_stride(), width, height),
                    u: copy_plane(
                        frame.u_plane(),
                        frame.u_stride(),
                        width.div_ceil(2),
                        height.div_ceil(2),
                    ),
                    v: copy_plane(
                        frame.v_plane(),
                        frame.v_stride(),
                        width.div_ceil(2),
                        height.div_ceil(2),
                    ),
                    width: width as i32,
                    height: height as i32,
                    pts_us,
                };
                context.backlog.fetch_add(1, Ordering::Relaxed);
                if context.video_tx.send(decoded).is_err() {
                    return Ok(());
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!(target: "moq", "failed to decode a video frame: {e}");
            }
        }
    }
}

/// 音声 Subgroup を復号する
async fn decode_audio_stream(
    mut stream: RecvStream,
    mut decoder: SubgroupStreamDecoder,
    stream_id: DataStreamId,
    context: StreamContext,
) -> Result<(), BoxError> {
    let Some(opus_decoder) = context.opus_decoder.as_ref() else {
        return drain_stream(&mut stream, &context.data_plane, stream_id).await;
    };
    let mut audio_config_checked = false;
    let mut last_pts_us = 0i64;
    loop {
        let Some(object) =
            next_subgroup_object(&mut stream, &mut decoder, &context.data_plane, stream_id).await?
        else {
            return Ok(());
        };
        let acceptance = context
            .data_plane
            .recv_subgroup_object(stream_id, &object)?;
        let Some(payload) =
            read_subgroup_payload(&mut stream, &mut decoder, &context.data_plane, stream_id)
                .await?
        else {
            return Ok(());
        };
        if !matches!(acceptance, TrackDataAcceptance::Accepted) {
            continue;
        }
        if object.status.is_some() || payload.is_empty() {
            continue;
        }

        let properties = parse_loc_properties(object.properties_bytes.as_deref())?;
        let pts_us = match compute_pts_us(properties.as_ref()) {
            Some(pts) => {
                last_pts_us = pts;
                pts
            }
            None => last_pts_us,
        };
        if !audio_config_checked
            && let Some(properties) = properties.as_ref()
            && let Some(config) = properties.audio_config()
        {
            // Audio Config は最初の 1 回だけ検証する。不整合でも再生は継続する
            match opus::parse_opus_head(config) {
                Ok(head) => {
                    let decoder = opus_decoder.lock().await;
                    if head.channel_count != decoder.channels() {
                        warn!(
                            target: "moq",
                            opus_head = head.channel_count,
                            catalog = decoder.channels(),
                            "Audio Config (OpusHead) channel count does not match the catalog"
                        );
                    }
                }
                Err(e) => {
                    warn!(target: "moq", "invalid OpusHead in Audio Config: {e}");
                }
            }
            audio_config_checked = true;
        }

        let mut decoder = opus_decoder.lock().await;
        match decoder.decode(&payload) {
            Ok(pcm) => {
                let frame = DecodedAudioFrame {
                    pcm,
                    sample_rate: decoder.sample_rate(),
                    channels: decoder.channels(),
                    pts_us,
                };
                if context.audio_tx.send(frame).is_err() {
                    return Ok(());
                }
            }
            Err(e) => {
                warn!(target: "moq", "failed to decode an audio packet: {e}");
            }
        }
    }
}

/// Subgroup の次の Object を読み出す
///
/// 終端の場合は `Ok(None)` を返す (Session への終端通知は済んでいる)。
async fn next_subgroup_object(
    stream: &mut RecvStream,
    decoder: &mut SubgroupStreamDecoder,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<Option<DecodedSubgroupObject>, BoxError> {
    loop {
        match decoder
            .try_decode_object()
            .map_err(|e| MoqError::Session(format!("failed to decode subgroup object: {e}")))?
        {
            Some(object) => return Ok(Some(object)),
            None => match receive_stream_data(stream, data_plane, stream_id).await? {
                StreamData::Data(data) => decoder.push(&data),
                StreamData::End => return Ok(None),
            },
        }
    }
}

/// Object の payload を読み出す
///
/// 終端の場合は `Ok(None)` を返す。
async fn read_subgroup_payload(
    stream: &mut RecvStream,
    decoder: &mut SubgroupStreamDecoder,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<Option<Vec<u8>>, BoxError> {
    loop {
        if let Some(payload) = decoder.try_read_payload() {
            return Ok(Some(payload));
        }
        match receive_stream_data(stream, data_plane, stream_id).await? {
            StreamData::Data(data) => decoder.push(&data),
            StreamData::End => return Ok(None),
        }
    }
}

/// ストリームから 1 チャンク読む。終端の場合は Session に通知して [`StreamData::End`] を返す
async fn receive_stream_data(
    stream: &mut RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<StreamData, BoxError> {
    match stream.receive_chunk().await? {
        RecvChunk::Data(data) => Ok(StreamData::Data(data)),
        RecvChunk::End(end) => {
            data_plane.recv_data_stream_closed(stream_id, end)?;
            Ok(StreamData::End)
        }
    }
}

/// ストリームを終端まで読み捨てる
async fn drain_stream(
    stream: &mut RecvStream,
    data_plane: &DataPlaneHandle,
    stream_id: DataStreamId,
) -> Result<(), BoxError> {
    loop {
        match receive_stream_data(stream, data_plane, stream_id).await? {
            StreamData::Data(_) => {}
            StreamData::End => return Ok(()),
        }
    }
}

/// ストリーム先頭の stream type varint を読む
///
/// 読み込んだバイト列は戻り値に含める。varint を消費してしまうと Subgroup / Fetch の
/// デコーダに先頭バイトを渡せないため、蓄積したバッファをそのまま引き継ぐ。
async fn peek_stream_type(stream: &mut RecvStream) -> Result<(u64, Vec<u8>), BoxError> {
    let mut decoder = MessageDecoder::new();
    let mut buf = Vec::new();
    loop {
        if let Some(value) = decoder
            .try_decode_varint()
            .map_err(|e| MoqError::Session(format!("failed to decode stream type: {e}")))?
        {
            return Ok((value, buf));
        }
        match stream.receive_chunk().await? {
            RecvChunk::Data(data) => {
                buf.extend_from_slice(&data);
                decoder.push(&data);
            }
            RecvChunk::End(_) => {
                return Err(MoqError::Session(
                    "data stream ended before the stream type".to_owned(),
                )
                .into());
            }
        }
    }
}

/// LOC プロパティをデコードする
fn parse_loc_properties(bytes: Option<&[u8]>) -> Result<Option<LocProperties>, BoxError> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let (properties, _) = LocProperties::decode(bytes)
        .map_err(|e| MoqError::Session(format!("failed to decode LOC properties: {e}")))?;
    Ok(Some(properties))
}

/// LOC の Timestamp / Timescale から表示時刻 (マイクロ秒) を計算する
///
/// Timescale が無い場合は Timestamp がマイクロ秒であるとみなす
/// (draft-ietf-moq-loc-04 §2.3.1.1)。draft 由来のため将来の改版で変わる可能性がある。
fn compute_pts_us(properties: Option<&LocProperties>) -> Option<i64> {
    let properties = properties?;
    let timestamp = properties.timestamp()?;
    let timescale = properties.timescale().unwrap_or(1_000_000);
    if timescale == 0 {
        return None;
    }
    let pts = timestamp as u128 * 1_000_000 / timescale as u128;
    Some(pts.min(i64::MAX as u128) as i64)
}

/// ストライド付きプレーンを詰めたプレーンにコピーする
fn copy_plane(src: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    for row in 0..height {
        let src_start = row.saturating_mul(stride);
        let dst_start = row * width;
        if src_start + width > src.len() {
            break;
        }
        out[dst_start..dst_start + width].copy_from_slice(&src[src_start..src_start + width]);
    }
    out
}

/// メインスレッドで raw_player のイベントループを実行する
///
/// macOS では SDL のウィンドウ操作をメインスレッドで行う必要があるため、この関数は
/// メインスレッドから呼ぶ。映像と音声の送信側が両方終了したら戻る。
pub fn run_player(
    video_rx: std::sync::mpsc::Receiver<DecodedVideoFrame>,
    audio_rx: std::sync::mpsc::Receiver<DecodedAudioFrame>,
    backlog: Arc<AtomicI64>,
) -> Result<(), BoxError> {
    raw_player::init()
        .map_err(|e| MoqError::Media(format!("failed to initialize raw_player: {e}")))?;
    info!(target: "moq", "player initialized");

    let mut video_player: Option<raw_player::VideoPlayer> = None;
    let audio_player = raw_player::AudioPlayer::new();
    let mut audio_started = false;
    let mut video_disconnected = false;
    let mut audio_disconnected = false;

    loop {
        if let Some(ref player) = video_player
            && !matches!(player.poll_events(), Ok(true))
        {
            info!(target: "moq", "player window closed");
            break;
        }

        match video_rx.try_recv() {
            Ok(frame) => {
                backlog.fetch_sub(1, Ordering::Relaxed);
                let need_recreate = match video_player.as_ref() {
                    None => true,
                    Some(player) => {
                        player.width() != frame.width || player.height() != frame.height
                    }
                };
                if need_recreate {
                    info!(target: "moq", width = frame.width, height = frame.height, "creating the player window");
                    let player = raw_player::VideoPlayer::new(
                        frame.width,
                        frame.height,
                        "momo - MoQT Subscriber",
                    )?;
                    player.play()?;
                    video_player = Some(player);
                }
                if let Some(ref player) = video_player
                    && let Err(e) = player.enqueue_video_i420(
                        &frame.y,
                        &frame.u,
                        &frame.v,
                        frame.width,
                        frame.height,
                        frame.pts_us,
                    )
                {
                    warn!(target: "moq", "failed to enqueue a video frame: {e}");
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if !video_disconnected {
                    info!(target: "moq", "video channel disconnected");
                    video_disconnected = true;
                }
            }
        }

        match audio_rx.try_recv() {
            Ok(frame) => {
                let bytes = pcm_i16_to_bytes(&frame.pcm);
                if let Err(e) = audio_player.enqueue_audio(
                    &bytes,
                    frame.pts_us,
                    frame.sample_rate as i32,
                    frame.channels as i32,
                    raw_player::AudioFormat::S16,
                ) {
                    warn!(target: "moq", "failed to enqueue an audio frame: {e}");
                }
                if !audio_started {
                    match audio_player.play() {
                        Ok(()) => {
                            audio_started = true;
                            info!(target: "moq", "audio playback started");
                        }
                        Err(e) => {
                            warn!(target: "moq", "failed to start audio playback: {e}");
                        }
                    }
                }
                if let Err(e) = audio_player.process() {
                    warn!(target: "moq", "failed to process the audio queue: {e}");
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if !audio_disconnected {
                    info!(target: "moq", "audio channel disconnected");
                    audio_disconnected = true;
                }
            }
        }

        if video_disconnected && audio_disconnected {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    drop(video_player);
    drop(audio_player);
    // SAFETY: プレイヤーの終了時に一度だけ呼ぶ
    unsafe { raw_player::quit() };
    Ok(())
}

/// `&[i16]` をリトルエンディアンのバイト列に変換する
fn pcm_i16_to_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(pcm.len() * 2);
    for sample in pcm {
        buf.extend_from_slice(&sample.to_le_bytes());
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_moqt::loc::{LocProperty, PROP_TIMESCALE, PROP_TIMESTAMP};

    /// Timestamp と Timescale からマイクロ秒を計算できること
    #[test]
    fn test_compute_pts_us() {
        let mut properties = LocProperties::new();
        properties.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: shiguredo_moqt::loc::LocPropertyValue::VarInt(90_000),
        });
        properties.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: shiguredo_moqt::loc::LocPropertyValue::VarInt(90_000),
        });
        assert_eq!(compute_pts_us(Some(&properties)), Some(1_000_000));
    }

    /// Timescale が無い場合はマイクロ秒として扱うこと
    #[test]
    fn test_compute_pts_us_without_timescale() {
        let mut properties = LocProperties::new();
        properties.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: shiguredo_moqt::loc::LocPropertyValue::VarInt(1_234_567),
        });
        assert_eq!(compute_pts_us(Some(&properties)), Some(1_234_567));
    }

    /// Timestamp が無い場合は None になること
    #[test]
    fn test_compute_pts_us_without_timestamp() {
        let properties = LocProperties::new();
        assert_eq!(compute_pts_us(Some(&properties)), None);
        assert_eq!(compute_pts_us(None), None);
    }

    /// ストライド付きプレーンを詰めてコピーできること
    #[test]
    fn test_copy_plane() {
        let src = [1u8, 2, 99, 3, 4, 99];
        assert_eq!(copy_plane(&src, 3, 2, 2), vec![1, 2, 3, 4]);
    }

    /// PCM をリトルエンディアンのバイト列に変換できること
    #[test]
    fn test_pcm_i16_to_bytes() {
        assert_eq!(pcm_i16_to_bytes(&[1, -1]), vec![1, 0, 0xFF, 0xFF]);
    }
}
