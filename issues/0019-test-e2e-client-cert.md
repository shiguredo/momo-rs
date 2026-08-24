# クライアント証明書認証の E2E テストを追加する

- Created: 2026-08-25
- Completed: {YYYY-MM-DD}
- Branch: feature/add-client-cert-e2e-test
- Polished: {YYYY-MM-DD}

## 目的

`--client-cert` / `--client-key` (Ayame / Sora モード) が実際に mTLS ハンドシェイクでサーバーへ提示されることを自動検証できるようにする。

## 根拠

- 実装は完了しているが、証明書を提示するだけでサーバー側が受け入れるかどうかの検証がされていない
- PEM パース (rustls-pki-types) や `with_client_auth_cert` / `SingleCertResolver` / `client_cert` の組み合わせは変更しやすく、回帰しても検出できない
- クライアント証明書は sorbed されるとハンドシェイク自体が失敗するため、mTLS が出ないと起動エラーも出ずに接続が失敗する形になる
- MOMO.md の「E2E テストで未カバーの領域」に記録済み

## 現状

- `--client-cert` / `--client-key` は Ayame (`src/ayame/signaling.rs` の rustls 接続構築) と Sora (`src/sora/mod.rs` で sora_sdk の `client_cert`) で実装済み
- `--client-cert` と `--client-key` の同時指定必須チェック (src/main.rs) は実装済み
- E2E テストが存在しない

## 設計方針

- 自己署名 CA でサーバー証明書とクライアント証明書を生成する pytest fixture を用意する
- mTLS を要求するシグナリングサーバーへの接続を検証する
  - Ayame: ローカルで mTLS を要求する Ayame 互換シグナリングサーバーを立てて接続確認
  - または Sora: mTLS 対応の Sora 接続先で検証 (対応サーバーの有無次第で範囲を確定する)

## 完了条件

- クライアント証明書を要求する WSS サーバーに対して `--client-cert` / `--client-key` を指定して接続が確立するテストが通る
- 証明書を提示しない場合に接続が拒否されることも確認する

## 依存

- クライアント証明書の実装は完了済み
- 検証用サーバーの準備 (Ayame 互換サーバーのローカル起動)
