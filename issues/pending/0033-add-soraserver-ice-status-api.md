# SoraServer の ICE 状態 API

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-soraserver-ice-status-api
- Polished: {YYYY-MM-DD}

## 目的

SoraServer の `GET /connect/status` を実装し、ICE 接続状態を momo と同じ文字列で返せるようにする。

## 現状

- SoraServer 自体が未実装
- Sora の PeerConnection は sora_sdk の内部にあり、momo-rs から observer を差し込めない
- `SoraConnectionEventHandler` に接続状態系のコールバックは無く、`SoraConnectionHandle` にも ICE 状態の取得口が無い

## 解決方法

- sora-rust-sdk に ICE 接続状態のイベントまたは取得 API を追加してもらい、それを `GET /connect/status` の応答に使う
- 応答は momo と同じ `new` / `checking` / `connected` / `completed` / `failed` / `disconnected` / `closed` / `max` / `unknown` の文字列にする

## 完了条件

`GET /connect/status` が ICE 接続状態を返す。

## pending 理由

sora-rust-sdk に Sora 接続の ICE 状態を取得する API が無く、上流の対応待ちのため。

## 関連

- SoraServer の HTTP サーバー基盤 (issue 0030) が前提
