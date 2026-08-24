# シリアルデータチャネルの E2E テストを追加する

- Created: 2026-08-25
- Completed: {YYYY-MM-DD}
- Branch: feature/add-serial-e2e-test
- Polished: {YYYY-MM-DD}

## 目的

`--serial` の DataChannel ↔ シリアルパススルーを自動検証できるようにする。`src/serial.rs` は外部依存なしの自作実装 (libc の termios + tokio AsyncFd) のため、シリアルの raw モード設定や「\n 等の境界の取り扱いを変えてしまうと壊れても検出できない。

## 根拠

- 対応ボーレート 17 種、raw モード設定、O_NONBLOCK、AsyncFd の読み書きと DataChannel の on_message 橋渡しはすべて自作実装
- MOMO.md の「E2E テストで未カバーの領域」に記録済み
- momo (C++) にも相当するテストが存在しないため、momo-rs で先に整備する

## 現状

- `src/serial.rs` に実装済み (Linux 限定、`#[cfg(target_os = "linux")]`)
- P2P (`src/p2p/webrtc.rs` の `on_data_channel`) と Ayame (`src/ayame/webrtc.rs` の `on_data_channel`) で label == "serial" の DataChannel をハンドリング (Sora は未対応)
- E2E テストが存在しない

## 設計方針

- 仮想シリアルポート (pty) を `socat` で作成し、raw デバイスの代わりに利用する
- P2P モードはブラウザクライアントが必要な構造のため、#0014 で整備する pytest-playwright 基盤を利用する
- 双方向を検証する:
  - DataChannel からのバイナリメッセージがシリアルポートへ書き込まれる
  - シリアルポートからのバイナリデータが DataChannel から受信できる

## 完了条件

- Linux CI で、P2P モード + 仮想シリアルポート + ブラウザクライアントによる DataChannel ↔ シリアルの双方向パススルーが検証できるテストが通る
- 対応ボーレート指定 (`--serial /dev/pts/N,9600` 等) の起動も検証する

## 依存

- #0014 (P2P モードの WebRTC 接続確立テスト、pytest-playwright 基盤) の完了を前提
