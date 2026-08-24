# SoraServer のミュート API

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-soraserver-mute-api
- Polished: {YYYY-MM-DD}

## 目的

SoraServer の `POST /mute` / `GET /mute/status` を実装し、HTTP API から送信トラックのミュートを制御できるようにする。

## 現状

- SoraServer 自体が未実装
- 送信トラックは momo-rs が `SoraConnectionContext` の `create_video_track` / `create_audio_track` で生成しており、複製を保持すれば接続後に `MediaStreamTrack::set_enabled` でミュートできる

## 解決方法

- `POST /mute`: body の `{"audio": bool, "video": bool}` に従って送信トラックをミュート / アンミュートする
- `GET /mute/status`: 現在のミュート状態を返す
- ミュート状態は接続をまたいで保持する

## 完了条件

`POST /mute` で音声・映像の送信をミュートでき、`GET /mute/status` で状態を取得できる。

## 関連

- SoraServer の HTTP サーバー基盤 (issue 0030) が前提
