# --hw-mjpeg-decoder が無視される

- Created: 2026-05-04
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-hw-mjpeg-decoder-ignored
- Polished: {YYYY-MM-DD}

## 目的

`--hw-mjpeg-decoder` が CLI でパースされるが、内部で `_hw_mjpeg_decoder` (アンダースコア付き) に束縛されるだけで破棄される。カメラ MJPEG 入力のハードウェアデコード設定が反映されない。

## 再現手順

1. MJPEG をサポートするカメラを接続する
2. `momo --hw-mjpeg-decoder true p2p ...` で HW MJPEG デコーダを有効化して起動する
3. CPU 使用率や動作上、ソフトウェアデコードと差がない

## 期待する動作

momo (C++) と同様に、`--hw-mjpeg-decoder true` で MJPEG のハードウェアデコードが有効化される (Raspberry Pi の V4L2 M2M デコーダ等)。

## 根拠

- `src/main.rs` の `main()` で `_hw_mjpeg_decoder: Option<String>` に束縛されているが利用されていない
- momo-cpp の `--hw-mjpeg-decoder` は V4L2 M2M デコーダ経由でカメラの MJPEG 出力をハードウェアデコードする

## 対応方針

- `_hw_mjpeg_decoder` のアンダースコアを除去し、MomoConfig に `hw_mjpeg_decoder: bool` を追加する
- カメラキャプチャパイプラインで V4L2 M2M デコーダ (libv4l2) または相当する仕組みで MJPEG → I420 変換を行う

## 分割経緯

元々コーデック選択の issue に含まれていたが、その pending 理由は HW エンコーダ未実装であり、MJPEG HW デコードはこれらとは独立して実装可能なため分離した。

## 参考

- momo-cpp の `--hw-mjpeg-decoder` 実装
- `src/main.rs` の `main()` の `--hw-mjpeg-decoder` 束縛
