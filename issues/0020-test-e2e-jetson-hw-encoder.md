# Jetson HW エンコーダーの E2E テストを追加する

- Created: 2026-08-25
- Completed: {YYYY-MM-DD}
- Branch: feature/add-jetson-hw-encoder-e2e-test
- Polished: {YYYY-MM-DD}

## 目的

Jetson の H.264/H.265 ハードウェアエンコーダー (#0002) の実装後に、実機でハードウェアエンコードが機能することを自動検証できるようにする。

## 根拠

- momo (C++) は Jetson の HW エンコーダー (USE_JETSON_ENCODER) を実機で利用しているが、momo-rs には HW エンコードの E2E テストがない
- NVIDIA NvCodec / Intel oneVPL は self-hosted runner で E2E を実施しているが、Jetson は未整備
- MOMO.md の「E2E テストで未カバーの領域」に記録済み

## 現状

- #0002 (Jetson の HW エンコーダー実装) は pending (未実装)
- #0012 (Jetson platform サポート) は open。Jetson 向け sysroot / rootfs はビルド面の整備として進んでいる
- Jetson での E2E テストは存在しない

## 設計方針

- #0002 の実装 (JetsonV4L2 / sora_sdk の Jetson コーデックcapability 等) を完了した上で、Jetson 実機 (self-hosted runner) に E2E テストを追加する
- 既存の NVIDIA NvCodec / Intel oneVPL E2E (`test_sora_mode_nvidia_video_codec.py` / `test_sora_mode_intel_vpl.py`) と同じ枠組みを利用する
- H.264 / H.265 の HW エンコードで sendonly 接続を確立し、encoderImplementation が Jetson HW であることを確認する

## 完了条件

- Jetson 実機の self-hosted runner で #0002 実装後、H.264 / H.265 の HW エンコードでの接続確立を検証する E2E テストが通る

## 依存

- #0002 (Jetson HW エンコーダー実装)
- #0012 (Jetson platform サポート、実機 runner の整備)
