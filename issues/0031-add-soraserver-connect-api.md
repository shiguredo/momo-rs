# SoraServer の接続制御 API

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-soraserver-connect-api
- Polished: {YYYY-MM-DD}

## 目的

SoraServer の `POST /connect` / `POST /close` を実装し、HTTP API から Sora 接続を開始・停止できるようにする。

## 現状

- SoraServer 自体が未実装
- 接続の停止は `SoraConnectionHandle::disconnect` と `sora::run` の停止フラグで実現できる

## 解決方法

- `POST /connect`: 未接続なら Sora 接続を開始する
- `POST /close`: 接続中の Sora を切断し、再接続を抑止する
- レスポンス形式は momo に合わせる

## 完了条件

`POST /connect` / `POST /close` で Sora 接続を開始・停止できる。

## 関連

- SoraServer の HTTP サーバー基盤 (issue 0030) が前提
