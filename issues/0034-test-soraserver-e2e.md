# SoraServer の E2E テスト

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-soraserver-e2e-test
- Polished: {YYYY-MM-DD}

## 目的

SoraServer の HTTP API を E2E テストで検証する。

## 現状

- SoraServer 自体が未実装
- `e2e-tests/momo.py` にメトリクス API を取得する仕組みがあり、HTTP API のテストに流用できる

## 解決方法

- SoraServer を起動し、`POST /connect` / `POST /close` / `POST /mute` / `GET /connect/status` / `GET /mute/status` を検証する
- Sora の実接続が必要なテストは、認証情報の環境変数が無い場合に skip する

## 完了条件

SoraServer の HTTP API の E2E テストが追加され、CI で実行される。

## 関連

- SoraServer の各 API の実装が前提
