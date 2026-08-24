# SoraServer の HTTP サーバー基盤

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-soraserver-http-server
- Polished: {YYYY-MM-DD}

## 目的

momo の SoraServer 相当の開発用 HTTP サーバーを momo-rs に追加する。Sora モードで HTTP API から接続を制御できるようにするための基盤を作る。

## 現状

- `--auto` は `src/main.rs` でパースされるだけで、SoraServer が無いため常に自動接続する動作になっている
- `--port` は削除済みで、SoraServer 用のポート指定は無い
- `src/metrics.rs` が shiguredo_http11 + tokio で HTTP/1.1 サーバーを実装しており、同じ仕組みを流用できる
- momo の SoraServer は 127.0.0.1 にバインドし、接続制御・ミュート・状態取得の API を提供する

## 解決方法

- SoraServer 用の `--port` (デフォルト値は momo に合わせて決める) を追加し、127.0.0.1 で HTTP サーバーを起動する
- `--auto` 指定時は起動時に Sora へ接続し、未指定時は接続 API を待つ
- HTTP サーバーは `src/metrics.rs` と同じ shiguredo_http11 + tokio の仕組みで実装する
- 接続の開始・停止は `sora::run` の停止フラグと `SoraConnectionHandle::disconnect` で制御する

## 完了条件

`--port` で SoraServer が起動し、`--auto` の指定に応じて接続の開始を制御できる。

## 関連

- momo の SoraServer 実装
