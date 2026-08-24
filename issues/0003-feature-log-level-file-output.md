# --log-level の実装とログファイル出力

- Created: 2026-03-22
- Completed: {YYYY-MM-DD}
- Branch: feature/add-log-file-output
- Polished: {YYYY-MM-DD}

## 目的

`--log-level` の適用とローテーション付きログファイル出力を実装する。

## 現状

- `--log-level` は実装済み (`src/main.rs` の `init_tracing()` で tracing の EnvFilter に変換)
- ログファイル出力は未実装。`tracing_subscriber::fmt().compact().init()` で stdout のみ
- momo は `webrtc::FileRotatingLogSink` で `webrtc_logs_*` に 10MB x 10 ファイルのローテーション出力

## 設計方針

ログファイル出力は `tracing_appender` の依存追加が必要なため、設計判断をした上で対応する。

## 解決方法

- ログファイル出力: tracing_appender 等でローテーション付きファイル出力

## 再オープン経緯

`--log-level` は実装済みで一度 closed されたが、ログファイル出力 (ローテーション付き) が未実装のままだったため reopened。momo は `FileRotatingLogSink` で 10MB x 10 ファイルのローテーション出力を行っており、この機能が残作業である。
