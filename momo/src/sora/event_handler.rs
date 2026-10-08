//! Sora 接続から届くイベントのハンドリング
//!
//! sora_sdk は接続のビルド時にイベントハンドラを 1 つのオブジェクトとして受け取る。
//! ここでは通知・プッシュ・シグナリング切替などのコールバックをログへ出し、
//! player feature が有効な場合は受信映像トラックの表示処理も受け持つ。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use shiguredo_webrtc::{RtpReceiver, RtpTransceiver};
use sora_sdk::{SignalingDirection, SignalingType, SoraConnectionEventHandler};
use tracing::{debug, info, trace, warn};

#[cfg(feature = "player")]
use shiguredo_webrtc::{VideoSink, VideoSinkWants, VideoTrack};

/// 接続中に保持する受信映像トラックのレジストリ
///
/// エントリの型は (トラック ID, VideoTrack, VideoSink)。
/// 受信トラックにアタッチしたシンクは、トラックが削除されるか接続が終了するまで
/// ここで生存させる必要がある。
#[cfg(feature = "player")]
pub type DisplayTrackRegistry = std::sync::Mutex<Vec<(String, VideoTrack, VideoSink)>>;

/// Sora 接続から届くイベントを処理するハンドラ
///
/// `SoraConnectionEventHandler` の実装。sora_sdk の各コールバックは
/// 単一タスクから直列に呼ばれるため、内部状態の排他はスレッドセーフであればよい。
pub struct SoraEventHandler {
    /// 接続が確立したかどうか
    ///
    /// シグナリング通知・プッシュ通知・トラック受信・WebSocket クローズはいずれも
    /// 接続確立後にしか発生しない。どれかを受信した時点で true を立て、
    /// 再接続回数を数え直す判定に使う。
    connected: Arc<AtomicBool>,
    /// 受信映像表示 (player) のコマンド送信チャネル
    #[cfg(feature = "player")]
    display_cmd_tx: Option<std::sync::mpsc::Sender<crate::display::DisplayCommand>>,
    /// 受信映像トラックのレジストリ
    ///
    /// レジストリは on_track / on_remove_track (WebRTC 内部スレッド) からのみ操作されるが、
    /// ハンドラ経由でしかアクセスできないため Mutex で保護する (ロック保持はごく短時間)。
    #[cfg(feature = "player")]
    track_registry: Arc<DisplayTrackRegistry>,
}

impl SoraEventHandler {
    /// イベントハンドラを生成する
    ///
    /// player feature が有効なときだけ受信映像表示へのコマンド送信チャネルを受け取る。
    pub fn new(
        connected: Arc<AtomicBool>,
        #[cfg(feature = "player")] display_cmd_tx: Option<
            std::sync::mpsc::Sender<crate::display::DisplayCommand>,
        >,
    ) -> Self {
        Self {
            connected,
            #[cfg(feature = "player")]
            display_cmd_tx,
            #[cfg(feature = "player")]
            track_registry: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// 接続が確立したことを記録する
    fn mark_connected(&self) {
        self.connected.store(true, Ordering::Relaxed);
    }

    /// 受信した映像トラックの表示を開始する
    ///
    /// 映像トラックに VideoSink をアタッチし、表示ウィンドウへ AddTrack コマンドを送る。
    /// アタッチした VideoSink はレジストリに保持し、接続終了まで生存させる。
    #[cfg(feature = "player")]
    fn handle_video_track(
        &mut self,
        transceiver: &RtpTransceiver,
        cmd_tx: &std::sync::mpsc::Sender<crate::display::DisplayCommand>,
    ) {
        let media_track = transceiver.receiver().track();
        let kind = match media_track.kind() {
            Ok(kind) => kind,
            Err(e) => {
                warn!(target: "sora", error = %e, "failed to get media track kind");
                return;
            }
        };
        if kind != "video" {
            info!(target: "sora", "audio track received");
            return;
        }
        let id = match media_track.id() {
            Ok(id) => id,
            Err(e) => {
                warn!(target: "sora", error = %e, "failed to get media track id");
                return;
            }
        };

        let video_track = media_track.cast_to_video_track();
        let (sink, rx) = crate::display::create_display_sink();
        video_track.add_or_update_sink(&sink, &VideoSinkWants::new());

        let mut entries = self
            .track_registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // 再ネゴシエーションで同一 ID が再追加された場合は古いものを取り除く
        // (アタッチを解除してから解放する)
        if let Some(index) = entries.iter().position(|(entry_id, _, _)| *entry_id == id) {
            let (_, old_track, old_sink) = entries.remove(index);
            old_track.remove_sink(&old_sink);
        }
        entries.push((id.clone(), video_track, sink));

        let _ = cmd_tx.send(crate::display::DisplayCommand::AddTrack { id, rx });
    }

    /// 受信した映像トラックの表示を終了する
    #[cfg(feature = "player")]
    fn handle_remove_video_track(
        &mut self,
        receiver: &RtpReceiver,
        cmd_tx: &std::sync::mpsc::Sender<crate::display::DisplayCommand>,
    ) {
        let media_track = receiver.track();
        let id = match media_track.id() {
            Ok(id) => id,
            Err(e) => {
                warn!(target: "sora", error = %e, "failed to get media track id");
                return;
            }
        };

        let mut entries = self
            .track_registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(index) = entries.iter().position(|(entry_id, _, _)| *entry_id == id) {
            let (_, track, sink) = entries.remove(index);
            // アタッチを解除してから解放する
            track.remove_sink(&sink);
        }

        let _ = cmd_tx.send(crate::display::DisplayCommand::RemoveTrack { id });
    }
}

impl SoraConnectionEventHandler for SoraEventHandler {
    /// シグナリングメッセージの送受信を出力する
    ///
    /// connect / offer / answer などを毎回流すとログが埋まるため trace にする。
    fn on_signaling_message(
        &mut self,
        signaling_type: SignalingType,
        direction: SignalingDirection,
        text: &str,
    ) {
        trace!(
            target: "sora",
            signaling = ?signaling_type,
            direction = ?direction,
            text = %text,
            "signaling message"
        );
    }

    /// シグナリング通知メッセージを出力する
    fn on_notify(&mut self, text: &str) {
        self.mark_connected();
        info!(target: "sora", notify = %text, "on_notify");
    }

    /// プッシュ通知メッセージを出力する
    fn on_push(&mut self, text: &str) {
        self.mark_connected();
        info!(target: "sora", push = %text, "on_push");
    }

    /// リモートトラックの受信を出力し、player が有効なら表示を開始する
    fn on_track(&mut self, transceiver: RtpTransceiver) {
        self.mark_connected();
        info!(target: "sora", "on_track");
        #[cfg(not(feature = "player"))]
        let _ = &transceiver;
        // Sender はクローン軽量なので、レジストリの可変参照と同時借用しないようコピーする
        #[cfg(feature = "player")]
        if let Some(cmd_tx) = self.display_cmd_tx.clone() {
            self.handle_video_track(&transceiver, &cmd_tx);
        }
    }

    /// リモートトラックの削除を出力し、player が有効なら表示を終了する
    fn on_remove_track(&mut self, receiver: RtpReceiver) {
        info!(target: "sora", "on_remove_track");
        #[cfg(not(feature = "player"))]
        let _ = &receiver;
        #[cfg(feature = "player")]
        if let Some(cmd_tx) = self.display_cmd_tx.clone() {
            self.handle_remove_video_track(&receiver, &cmd_tx);
        }
    }

    /// WebSocket から DataChannel シグナリングへの切替完了を出力する
    fn on_switched(&mut self) {
        self.mark_connected();
        info!(target: "sora", "switched signaling from websocket to data channel");
    }

    /// WebSocket のクローズを出力する
    ///
    /// `--ignore-disconnect-websocket` の挙動確認に使うため、コードと理由を必ず残す。
    fn on_websocket_close(&mut self, code: Option<u16>, reason: &str) {
        self.mark_connected();
        warn!(target: "sora", code = ?code, reason = %reason, "websocket closed");
    }

    /// `#` プレフィックス付きのユーザー定義 DataChannel のメッセージを出力する
    fn on_message(&mut self, label: &str, data: &[u8]) {
        info!(
            target: "sora",
            label = %label,
            length = data.len(),
            "user data channel message"
        );
    }

    /// サーバーから DataChannel が作成されたことを出力する
    fn on_data_channel(&mut self, label: &str) {
        info!(target: "sora", label = %label, "data channel created");
    }

    /// DataChannel が開かれたことを出力する
    fn on_data_channel_open(&mut self, label: &str) {
        info!(target: "sora", label = %label, "data channel opened");
    }

    /// DataChannel で受けたメッセージを出力する
    ///
    /// シグナリング用の DataChannel も含まれるため、内容を出さない debug に留める。
    /// ユーザー定義ラベルは on_message が info で出力する。
    fn on_data_channel_message(&mut self, label: &str, data: &[u8]) {
        debug!(
            target: "sora",
            label = %label,
            length = data.len(),
            "data channel message"
        );
    }

    /// DataChannel が閉じられたことを出力する
    fn on_data_channel_close(&mut self, label: &str) {
        info!(target: "sora", label = %label, "data channel closed");
    }
}
