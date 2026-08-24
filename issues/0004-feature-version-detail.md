# --version の詳細表示

- Created: 2026-03-22
- Completed: {YYYY-MM-DD}
- Branch: feature/add-build-flags-display
- Polished: {YYYY-MM-DD}

## 目的

`--version` の出力にビルドフラグ (有効な feature 一覧) を表示する。

## 現状

- コミットハッシュ、libwebrtc バージョン、環境情報は実装済み (`src/main.rs` の `main()` の `--version` 処理、`metrics::build_environment_string()`)
- Build Flags 表示は実装済みだが、player feature などの表示漏れがある (修正済みの領域)
- momo は USE_JETSON_ENCODER / USE_NVCODEC_ENCODER / USE_V4L2_ENCODER / USE_VPL_ENCODER 等のコンパイルオプション一覧を表示している

## 解決方法

- ビルドフラグ表示: 有効な feature 一覧を表示する (表示対象フラグの設計判断が必要)

## 再オープン経緯

コミットハッシュ、libwebrtc バージョン、環境情報は実装済みで一度 closed されたが、ビルドフラグ表示が未実装だったため reopened。
