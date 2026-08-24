# プロキシ対応の E2E テストを追加する

- Created: 2026-08-25
- Completed: {YYYY-MM-DD}
- Branch: feature/add-proxy-e2e-test
- Polished: {YYYY-MM-DD}

## 目的

`--proxy-url` / `--proxy-username` / `--proxy-password` (Sora モードのみ。sora_sdk の `ProxyInfo` API 経由) が実際に機能することを自動検証できるようにする。

## 根拠

- プロキシ関連の CLI オプションがパースされて sora_sdk に渡るだけで、実際に CONNECT トンネリングが機能するかの検証がない
- プロキシ経由時にだけ発生する問題 (認証ヘッダーの欠落、CONNECT 応答のステータス未検証等) が回帰しても検出できない
- MOMO.md の「E2E テストで未カバーの領域」に記録済み

## 現状

- `src/sora.rs` で `ProxyInfo` (proxy_url / proxy_username / proxy_password) を sora_sdk に渡す実装は完了済み
- sora_sdk 内部での CONNECT トンネリングの挙動は sora_sdk 側実装のため、momo-rs 単体のテストでは検証不可能
- 実装時に P2P / Ayame は非対応とされている

## 設計方針

- ローカルに CONNECT トンネルを実行する簡易 HTTP プロキシ (Python 標準ライブラリ / pytest fixture) を立てる
- 被プロキシの WSS 接続先 (Sora のシグナリング URL) を `TEST_SORA_MODE_SIGNALING_URLS` で与え、プロキシ経由で接続する
- Basic 認証付き (URL 以外に username / password 指定) と認証なしの両ケースを検証する
- プロキシ側で CONNECT リクエストと値をログし、実際にトンネリングされたことを確認する

## 完了条件

- ローカルプロキシ経由で Sora モードの WebRTC 接続が確立する E2E テストが通る
- プロキシログで CONNECT リクエスト (Basic 認証付き / なしの双方) が到達したことを確認できる

## 依存

- 既存の Sora モード E2E 基盤 (`e2e-tests/conftest.py` の `sora_settings`、`TEST_SORA_MODE_SIGNALING_URLS`)
