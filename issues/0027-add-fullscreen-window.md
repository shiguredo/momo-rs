# --fullscreen の実装

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-fullscreen-window
- Polished: {YYYY-MM-DD}

## 目的

momo (C++) と同様に、受信映像をフルスクリーンのウィンドウで表示できるようにする。momo-rs の `--fullscreen` は CLI で受け付けるだけで、表示処理に反映されていない。

## 現状

- `src/main.rs` の `--fullscreen` は `_fullscreen` に束縛されるだけで、`MomoConfig` にも表示処理にも渡されていない
- 受信映像の表示は `src/display.rs` の `run_display_loop` が担い、`Window::new("momo", window_width, window_height)` で通常ウィンドウを生成する
- 依存する raw_player 2026.2.0 の `Window` にはフルスクリーン設定 API が無く、`raw_player::sys` の ffi にも `SDL_SetWindowFullscreen` 相当のバインディングが無い
- momo は起動時の指定に加え、実行中に F キーでフルスクリーンをトグルできる

## 完了条件

`--player --fullscreen` で起動したとき、受信映像のウィンドウがフルスクリーン表示になる。

## 解決方法

- raw-player-rs の `Window` にフルスクリーンを設定する API (例: `Window::set_fullscreen(bool)`) を追加する。SDL3 の `SDL_SetWindowFullscreen` を ffi にバインドする必要がある
- `src/display.rs` の `run_display_loop` に `fullscreen: bool` を追加し、ウィンドウ生成後にフルスクリーンへ切り替える
- `src/main.rs` の `_fullscreen` を `fullscreen` にリネームし、`MomoConfig` 経由で表示処理へ配線する
- F キーでのトグルは momo 互換のため raw-player-rs のイベント API と組み合わせて実装する。初回対応は起動時指定のみでもよい

## 参考

- docs/MOMO.md の「表示」セクション
- raw-player-rs の `src/window.rs`
