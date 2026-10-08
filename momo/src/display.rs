//! 受信映像の表示 (player) モジュール
//!
//! Sora で受信した複数の映像トラックを 1 つの SDL3 ウィンドウに
//! グリッドレイアウトでリアルタイム表示する。ローカルプレビューは表示しない。

use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Duration;

use raw_player::{Event, KEYCODE_ESCAPE, Renderer, Texture, Window};
use shiguredo_webrtc::{I420Buffer, VideoFrameRef, VideoSink, VideoSinkHandler};

use crate::error::BoxError;

// ─── 表示フレーム ──────────────────────────────────────────────────────────────

/// I420 プレーンのコピーを保持する表示フレーム
pub struct DisplayFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

// ─── 表示コマンド ──────────────────────────────────────────────────────────────

/// 表示ウィンドウ (メインスレッド) へ送る操作コマンド
pub enum DisplayCommand {
    /// 映像トラックの追加。フレームは `rx` 経由で届く
    AddTrack {
        /// トラック ID (グリッドのセルを追跡するための一意な ID)
        id: String,
        /// 受信フレームの受信チャネル
        rx: Receiver<DisplayFrame>,
    },
    /// 映像トラックの削除
    RemoveTrack {
        /// トラック ID
        id: String,
    },
}

// ─── VideoSink ─────────────────────────────────────────────────────────────────

/// VideoTrack にアタッチしてフレームを表示ループへ送るシンク
///
/// 各ストリームごとに 1 つの `FrameSinkHandler` を生成する。
/// 容量 1 の bounded チャネルで最新フレームのみを保持し、あふれたフレームは
/// ベストエフォートでドロップする。表示の欠落は許容する。
struct FrameSinkHandler {
    tx: SyncSender<DisplayFrame>,
}

impl VideoSinkHandler for FrameSinkHandler {
    fn on_frame(&mut self, frame: VideoFrameRef<'_>) {
        let Some(buffer) = frame.buffer().as_i420() else {
            return;
        };
        let display_frame = extract_display_frame(&buffer);
        let _ = self.tx.try_send(display_frame);
    }
}

/// 受信映像トラックにアタッチする `VideoSink` とフレーム受信チャネルを生成する
///
/// 返された `VideoSink` はアタッチを解除するまで生きていなければならない。
/// 解除は `shiguredo_webrtc::VideoTrack::remove_sink` で行う。
pub fn create_display_sink() -> (VideoSink, Receiver<DisplayFrame>) {
    let (tx, rx) = std::sync::mpsc::sync_channel::<DisplayFrame>(1);
    let sink = VideoSink::new_with_handler(Box::new(FrameSinkHandler { tx }));
    (sink, rx)
}

// ─── I420 フレーム抽出 ─────────────────────────────────────────────────────────

/// I420Buffer から Y/U/V プレーンを stride 考慮でコピーして DisplayFrame を生成する
///
/// SDL3 の I420 テクスチャは奇数幅・奇数高を受け付けないため、偶数へ切り捨てる。
pub fn extract_display_frame(buf: &I420Buffer) -> DisplayFrame {
    let width = buf.width() & !1;
    let height = buf.height() & !1;
    let chroma_w = width / 2;
    let chroma_h = height / 2;

    let mut y = vec![0u8; (width as usize) * (height as usize)];
    let mut u = vec![0u8; (chroma_w as usize) * (chroma_h as usize)];
    let mut v = vec![0u8; (chroma_w as usize) * (chroma_h as usize)];

    let stride_y = buf.stride_y() as usize;
    let stride_u = buf.stride_u() as usize;
    let stride_v = buf.stride_v() as usize;
    let y_data = buf.y_data();
    let u_data = buf.u_data();
    let v_data = buf.v_data();

    // Y プレーン: 行ごとに width バイトコピー
    for row in 0..(height as usize) {
        let src = &y_data[row * stride_y..row * stride_y + width as usize];
        y[row * (width as usize)..(row + 1) * (width as usize)].copy_from_slice(src);
    }

    // U/V プレーン
    for row in 0..(chroma_h as usize) {
        let u_src = &u_data[row * stride_u..row * stride_u + chroma_w as usize];
        u[row * (chroma_w as usize)..(row + 1) * (chroma_w as usize)].copy_from_slice(u_src);
        let v_src = &v_data[row * stride_v..row * stride_v + chroma_w as usize];
        v[row * (chroma_w as usize)..(row + 1) * (chroma_w as usize)].copy_from_slice(v_src);
    }

    DisplayFrame {
        y,
        u,
        v,
        width,
        height,
    }
}

// ─── 表示イベントループ ────────────────────────────────────────────────────────

/// 1 つのセル (映像トラック) の状態
struct Cell {
    id: String,
    rx: Receiver<DisplayFrame>,
    latest: Option<DisplayFrame>,
    texture: Option<Texture>,
}

/// メインスレッドで SDL3 イベントループを実行し、受信映像をグリッド表示する
///
/// - `cmd_rx` からトラックの追加・削除コマンドを受信する
/// - VideoSink から届いたフレームを最新 1 件ずつテクスチャへ反映する
/// - `window_width` / `window_height` でグリッドを自動計算して各セルへ
///   アスペクト比を保って配置する
/// - `shutdown_rx` で Sora 接続終了を検知して終了する
///
/// VideoSink の解除 (remove_sink) は Sora 側で行うため、この関数では
/// 受信チャネルとテクスチャの解放だけを行う。
pub fn run_display_loop(
    window_width: i32,
    window_height: i32,
    cmd_rx: Receiver<DisplayCommand>,
    shutdown_rx: Receiver<()>,
) -> Result<(), BoxError> {
    raw_player::init().map_err(|e| -> BoxError { format!("SDL3 init failed: {e}").into() })?;

    let window = Window::new("momo", window_width, window_height)
        .map_err(|e| -> BoxError { format!("Window の生成に失敗: {e}").into() })?;
    let mut renderer = Renderer::new_gpu(&window)
        .map_err(|e| -> BoxError { format!("Renderer の生成に失敗: {e}").into() })?;
    renderer
        .set_vsync(1)
        .map_err(|e| -> BoxError { format!("vsync の設定に失敗: {e}").into() })?;

    // リソースは Drop 順序 (逆順) で cells → renderer → window の順に解放される
    let mut cells: Vec<Cell> = Vec::new();

    loop {
        // Sora 接続終了の検知
        if shutdown_rx.try_recv().is_ok() {
            break;
        }

        // トラック追加・削除コマンドの処理 (ノンブロッキング)
        while let Ok(cmd) = cmd_rx.try_recv() {
            handle_display_command(&mut cells, cmd);
        }

        // ウィンドウが閉じられた場合も Sora 接続は継続するため終了する
        if window_close_requested() {
            break;
        }

        // グリッド描画 (ウィンドウサイズはウィンドウから毎回取得する)
        let (win_w, win_h) = window.size();
        if let Err(e) = render_grid(&mut cells, &mut renderer, win_w, win_h) {
            tracing::warn!(target: "display", error = %e, "grid render failed");
            break;
        }

        std::thread::sleep(Duration::from_millis(1));
    }

    drop(cells);
    drop(renderer);
    drop(window);
    // SAFETY: SDL3 の初期化・終了はこの関数内で対になっており、全リソースの
    // drop 後に呼ぶので安全である。
    unsafe { raw_player::quit() };
    Ok(())
}

/// トラック追加・削除コマンドを処理する
fn handle_display_command(cells: &mut Vec<Cell>, cmd: DisplayCommand) {
    match cmd {
        DisplayCommand::AddTrack { id, rx } => {
            // 再ネゴシエーションで同一 ID が再追加された場合は置き換える
            if let Some(index) = cells.iter().position(|cell| cell.id == id) {
                cells.remove(index);
            }
            cells.push(Cell {
                id,
                rx,
                latest: None,
                texture: None,
            });
        }
        DisplayCommand::RemoveTrack { id } => {
            if let Some(index) = cells.iter().position(|cell| cell.id == id) {
                cells.remove(index);
            }
        }
    }
}

/// ウィンドウクローズ・Quit・ESC キーによる終了要求を確認する
fn window_close_requested() -> bool {
    let mut close = false;
    while let Some(event) = raw_player::poll_event() {
        match event {
            Event::Quit | Event::WindowClose => close = true,
            Event::KeyDown { keycode } if keycode == KEYCODE_ESCAPE => close = true,
            _ => {}
        }
    }
    close
}

/// 全セルをグリッドレイアウトで描画する
fn render_grid(
    cells: &mut [Cell],
    renderer: &mut Renderer,
    win_w: i32,
    win_h: i32,
) -> Result<(), BoxError> {
    renderer
        .set_draw_color(0, 0, 0, 255)
        .map_err(|e| -> BoxError { format!("set_draw_color に失敗: {e}").into() })?;
    renderer
        .clear()
        .map_err(|e| -> BoxError { format!("clear に失敗: {e}").into() })?;

    if cells.is_empty() {
        renderer
            .present()
            .map_err(|e| -> BoxError { format!("present に失敗: {e}").into() })?;
        return Ok(());
    }

    // セル間マージン (ピクセル)
    let gap = 8.0f32;

    let cols = grid_cols(cells.len(), win_w, win_h);
    let rows = cells.len().div_ceil(cols);

    let cell_w = (win_w as f32 - gap * (cols as f32 + 1.0)) / cols as f32;
    let cell_h = (win_h as f32 - gap * (rows as f32 + 1.0)) / rows as f32;

    for (index, cell) in cells.iter_mut().enumerate() {
        let col = index % cols;
        let row = index / cols;
        let x = gap + (col as f32) * (cell_w + gap);
        let y = gap + (row as f32) * (cell_h + gap);

        // セルの背景
        renderer
            .set_draw_color(32, 32, 32, 255)
            .map_err(|e| -> BoxError { format!("set_draw_color に失敗: {e}").into() })?;
        renderer
            .fill_rect(x, y, cell_w, cell_h)
            .map_err(|e| -> BoxError { format!("fill_rect に失敗: {e}").into() })?;

        // 最新フレームのみ保持する (あふれたフレームはドロップ済み)
        while let Ok(frame) = cell.rx.try_recv() {
            cell.latest = Some(frame);
        }

        let Some(ref frame) = cell.latest else {
            continue;
        };

        // フレームサイズが変わった場合はテクスチャを作り直す
        let needs_texture = match &cell.texture {
            Some(texture) => texture.width() != frame.width || texture.height() != frame.height,
            None => true,
        };
        if needs_texture {
            cell.texture = Some(
                Texture::new_yuv(renderer, frame.width, frame.height).map_err(|e| -> BoxError {
                    format!("Texture の生成に失敗: {e}").into()
                })?,
            );
        }

        let Some(ref mut texture) = cell.texture else {
            continue;
        };
        texture
            .update_yuv(
                &frame.y,
                frame.width,
                &frame.u,
                frame.width / 2,
                &frame.v,
                frame.width / 2,
            )
            .map_err(|e| -> BoxError { format!("update_yuv に失敗: {e}").into() })?;

        // アスペクト比を保ってセル内に収める
        let scale = (cell_w / frame.width as f32).min(cell_h / frame.height as f32);
        let dst_w = frame.width as f32 * scale;
        let dst_h = frame.height as f32 * scale;
        let dst_rect = raw_player::sys::SDL_FRect {
            x: x + (cell_w - dst_w) / 2.0,
            y: y + (cell_h - dst_h) / 2.0,
            w: dst_w,
            h: dst_h,
        };

        // SAFETY: レンダラーとテクスチャはこのループ内で生成されたものであり、
        // メインスレッドからしかアクセスしない。srcrect に null を渡すと
        // テクスチャ全体が描画される (SDL3 の仕様) ためポインタは有効である。
        let ok = unsafe {
            raw_player::sys::SDL_RenderTexture(
                renderer.as_ptr(),
                texture.as_ptr(),
                std::ptr::null(),
                &dst_rect,
            )
        };
        if !ok {
            tracing::warn!(target: "display", "SDL_RenderTexture failed");
        }
    }

    renderer
        .present()
        .map_err(|e| -> BoxError { format!("present に失敗: {e}").into() })?;
    Ok(())
}

/// セル数をウィンドウアスペクト比に合わせて列数を計算する
fn grid_cols(count: usize, win_w: i32, win_h: i32) -> usize {
    // ウィンドウの縦横比が大きいほど 1 行のセル数を増やす
    let cols = ((count as f32) * (win_w as f32) / (win_h as f32).max(1.0)).sqrt();
    (cols.round() as usize).clamp(1, count.max(1))
}
