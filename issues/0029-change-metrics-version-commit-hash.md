# メトリクス API の version にコミットハッシュを含める

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/change-metrics-version-commit-hash
- Polished: {YYYY-MM-DD}

## 目的

momo (C++) のメトリクス API の `version` はバージョンとコミットハッシュを含む。momo-rs はバージョンのみで、どのバイナリが動作しているかを判別できない。momo 互換のためコミットハッシュを含める。

## 現状

- `src/metrics.rs` の `MetricsState::new` は `format!("WebRTC Native Client Momo {}", env!("CARGO_PKG_VERSION"))` で `version` を組み立てる
- コミットハッシュは `build.rs` が `MOMO_COMMIT_SHORT` として埋め込み、`--version` の表示で使用している
- momo の形式は `"WebRTC Native Client Momo {VERSION} ({COMMIT_SHORT})"`

## 完了条件

`GET /metrics` の `version` が `"WebRTC Native Client Momo {VERSION} ({COMMIT_SHORT})"` 形式になる。

## 解決方法

- `src/metrics.rs` の `MetricsState::new` で `env!("MOMO_COMMIT_SHORT")` を含める
- `e2e-tests/test_metrics_api.py` の `test_metrics_version_format` を新しい形式に合わせて更新する

## 参考

- docs/MOMO.md の「メトリクス API」セクション
