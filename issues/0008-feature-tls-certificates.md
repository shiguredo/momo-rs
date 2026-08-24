# TLS 証明書オプションの実装 (--cacert / --client-cert / --client-key)

- Created: 2026-03-22
- Completed: {YYYY-MM-DD}
- Branch: feature/add-tls-certificates
- Polished: {YYYY-MM-DD}

## 目的

CA 証明書、クライアント証明書、クライアント秘密鍵の指定オプションを Ayame / Sora モードの TLS 接続に反映する。

## 根拠

自己署名 CA やクライアント認証を使うサーバーへの接続に必要。momo では `--client-cert` / `--client-key` が実装済み。momo-rs では `--cacert` を新規追加し、curl と同じ命名に合わせる。

## momo との互換性

| オプション | momo | momo-rs | 備考 |
|-----------|------|---------|------|
| `--client-cert` | 実装済み | CLI のみ | PEM 形式クライアント証明書 |
| `--client-key` | 実装済み | CLI のみ | PEM 形式秘密鍵 |
| `--cacert` | **なし** | 新規追加 | CA 証明書。momo にはないため momo-rs 独自機能 |

`--cacert` は momo にないオプションのため、momo との非互換が発生する。

## 解決方法

Sora モード:

- sora_sdk の builder API
  - `.client_cert(cert_pem, key_pem)`: クライアント証明書
  - `.ca_cert(cert_pem)`: CA 証明書

Ayame モード:

- rustls の ClientConfig
  - `with_client_auth_cert()`: クライアント証明書
  - カスタム `RootCertStore`: CA 証明書

共通:

- `--cacert` CLI オプションの追加 (PEM 形式ファイルパス)
- `_client_cert` / `_client_key` のアンダースコア除去
- MomoConfig に `client_cert` / `client_key` / `cacert` を追加
- ファイル読み込みとバリデーション
