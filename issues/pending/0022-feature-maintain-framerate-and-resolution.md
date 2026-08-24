# maintain-framerate-and-resolution の degradation preference をサポートする

- Created: 2026-08-25
- Completed: {YYYY-MM-DD}
- Branch: feature/add-maintain-framerate-and-resolution
- Polished: {YYYY-MM-DD}

## 目的

libwebrtc の `DegradationPreference::MaintainFramerateAndResolution` (フレームレートと解像度の両方を維持し、リソース不足時はフレームドロップで対処する) を `--degradation-preference` から指定できるようにする。

## 現状

- `--degradation-preference` は balanced / maintain-framerate / maintain-resolution のみを受け付ける
- shiguredo_webrtc の `DegradationPreference` には `MaintainFramerateAndResolution` が存在しない (0.150.3 / 0.151.0 で確認。ffi バインドにも対応定数なし)
- sora_sdk には degradation preference を設定する API 自体が存在しない (2026.1.0-canary.21 まで確認)
- Sora モードでは `--degradation-preference` の明示指定に起動時エラーを返している

## 完了条件

- shiguredo_webrtc に `DegradationPreference::MaintainFramerateAndResolution` が追加されたら、`--degradation-preference` の選択肢に `maintain-framerate-and-resolution` を追加し P2P / Ayame モードで適用する
- sora-rust-sdk に degradation preference API が追加されたら、Sora モードにも適用し、Sora モードの起動時エラーを削除する

## 解決方法

- webrtc-rs (shiguredo/webrtc-rs) 側: ffi シェルに `MAINTAIN_FRAMERATE_AND_RESOLUTION` 定数を追加し、`DegradationPreference` enum に `MaintainFramerateAndResolution` を追加する
- sora-rust-sdk (shiguredo/sora-rust-sdk) 側: degradation preference を設定する API を追加する
- momo-rs 側: 両クレートの更新後に `--degradation-preference` のマッピングと Sora のエラー処理を修正する

## pending 理由

外部依存 (webrtc-rs / sora-rust-sdk) の API 追加が完了するまで実装できないため pending にする。両クレートの更新は時雨堂のパッケージ更新フローに依存する。
