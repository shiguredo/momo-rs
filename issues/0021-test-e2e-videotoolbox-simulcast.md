# VideoToolbox HWA simulcast の E2E テストを追加する

- Created: 2026-08-25
- Completed: {YYYY-MM-DD}
- Branch: feature/add-videotoolbox-simulcast-e2e-test
- Polished: {YYYY-MM-DD}

## 目的

Apple Video Toolbox (HWA) を使った simulcast の送信が機能するようになったら、skip されている E2E テストを有効にし、VideoToolbox との組み合わせでの simulcast を自動検証できるようにする。

## 根拠

- `e2e-tests/test_sora_mode_apple_video_toolbox.py` の `test_simulcast` が webrtc-rs の HWA での simulcast 未対応のため `pytest.mark.skip` されている
- ソフトウェアエンコーダーでの simulcast はテスト済みだが、HWA + simulcast は適用できていないためカバレッジの穴になっている
- MOMO.md の「E2E テストで未カバーの領域」に記録済み

## 現状

- `e2e-tests/test_sora_mode_apple_video_toolbox.py` の `test_simulcast` (test_connection_stats と sendonly/recvonly ペアのテストは有効)
- skip の理由: `webrtc-rs が HWA での simulcast に未対応`。webrtc-rs 側の対応状況は不明

## 設計方針

- webrtc-rs (shiguredo_webrtc) の HWA (VideoToolbox) simulcast 対応状況を上流 (webrtc-rs / shiguredo-webrtc) を確認する
- 対応が完了している場合は `test_simulcast` の skip を外し、simulcast エンコーディングの確認 (encoderImplementation が VideoToolbox であること等) を検証する
- 未対応の場合は webrtc-rs 側への対応要望を検討し、その結果を待つ

## 完了条件

- macOS (Apple Video Toolbox) 環境で HWA エンコーダーの simulcast 接続が確立し、stats 上で 3 レイヤー分の送信 (SimulcastEncoderAdapter 等) が確認できる E2E テストが通る

## 依存

- webrtc-rs (shiguredo_webrtc) の HWA simulcast 対応 (上流対応の進捗次第)
