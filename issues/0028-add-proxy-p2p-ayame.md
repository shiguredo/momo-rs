# P2P / Ayame モードのプロキシ対応

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-proxy-p2p-ayame
- Polished: {YYYY-MM-DD}

## 目的

momo (C++) は P2P / Ayame モードでもプロキシを経由して接続できる。momo-rs の `--proxy-url` / `--proxy-username` / `--proxy-password` は Sora モードでのみ反映され、P2P / Ayame モードでは無視される。momo との互換性のために対応する。

## 現状

- `src/main.rs` でパースした `proxy_url` / `proxy_username` / `proxy_password` は `MomoConfig` に格納される
- Sora モードでは `src/sora.rs` の `SoraConfig` から `SoraClientBuilder::proxy(ProxyInfo)` に渡される
- P2P モードは `src/p2p/websocket.rs` の `WebSocketServerConnection` で接続を受け付けるサーバーであり、外向きの WebSocket 接続はない
- Ayame モードは `src/ayame/signaling.rs` で `TcpStream::connect` と TLS 接続を行い、`WebSocketClientConnection` でシグナリングする。プロキシは経由しない
- 依存する shiguredo_websocket 2026.3.0 にはプロキシ機能がない
- ICE / STUN も P2P / Ayame モードではプロキシを経由しない

## 完了条件

`--proxy-url` などを指定して P2P / Ayame モードを起動したとき、Ayame のシグナリングと P2P / Ayame の ICE / STUN がプロキシ経由になる。

## 解決方法

- Ayame モードの WebSocket 接続: `src/ayame/signaling.rs` の TCP 接続前に HTTP CONNECT でプロキシへトンネルし、そのストリームに TLS / WebSocket を載せる (Sora モードのプロキシ実装と同等の処理を momo-rs 側に持つ)
- ICE / STUN: shiguredo_webrtc の `PeerConnectionDependencies::set_proxy` と `PeerConnectionFactory::default_network_manager` / `default_socket_factory` を使ってプロキシを設定する
- P2P モードはシグナリングがサーバー側のため、ICE / STUN のプロキシのみ対応する
- `--proxy-username` / `--proxy-password` は Basic 認証としてプロキシへ送る

## 参考

- docs/MOMO.md の「ネットワーク・セキュリティ」セクション
- shiguredo_webrtc の `PeerConnectionDependencies::set_proxy`
