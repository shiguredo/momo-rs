# 音声処理オプションの実装

- Created: 2026-03-22
- Completed: {YYYY-MM-DD}
- Branch: feature/add-audio-processing-options
- Polished: {YYYY-MM-DD}

## 目的

音声処理オプション (AEC/AGC/NS/ハイパスフィルター) を WebRTC の AudioProcessing 設定に反映する。

## 現状

- `src/main.rs` の `main()` で `--disable-echo-cancellation` / `--disable-auto-gain-control` / `--disable-noise-suppression` / `--disable-highpass-filter` が `_` 付き変数に束縛されるだけで未使用
- MomoConfig に含まれず、WebRTC AudioProcessing に渡されていない

## 解決方法

- `--disable-echo-cancellation`: WebRTC BuiltinAudioProcessing のエコーキャンセレーション無効化
- `--disable-auto-gain-control`: 自動ゲイン制御無効化
- `--disable-noise-suppression`: ノイズ抑制無効化
- `--disable-highpass-filter`: ハイパスフィルター無効化
- MomoConfig に追加し各モードに渡す

## pending 理由

shiguredo_webrtc (0.146.0-canary.4) の `AudioProcessingBuilder` に echo cancellation / AGC / noise suppression / highpass filter の設定 API が公開されていない。`webrtc::BuiltinAudioProcessingBuilder` の C API ラッパーに SetConfig 相当の関数が追加されるまで実装不可。

## 関連

- `--audio-output-device` が無視される問題: 元々この issue に含まれていたが、cpal 等の出力デバイス選択は AudioProcessing API とは独立して実装可能なため分離した
