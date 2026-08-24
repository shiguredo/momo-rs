# H.264 HW エンコード (Jetson) の実装

- Created: 2026-03-22
- Completed: {YYYY-MM-DD}
- Branch: feature/add-jetson-hw-encoder
- Polished: {YYYY-MM-DD}

## 目的

NVIDIA Jetson ハードウェアエンコーダー (H.264/H.265) を WebRTC に統合する。

## 現状

- momo は USE_JETSON_ENCODER コンパイルオプションで対応
- momo-rs には該当実装なし

## 解決方法

- Jetson ハードウェアエンコーダーの WebRTC 統合
- VideoEncoderFactory への組み込み

## pending 理由

Jetson 向けの HW エンコーダー API (libv4l2 / V4L2 M2M 等) の利用可否と、クロスコンパイル環境 (issue 0012) の建設計画が分からないため保留。
