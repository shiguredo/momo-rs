# Sora モードの自動再接続

- Created: 2026-09-07
- Completed: {YYYY-MM-DD}
- Branch: feature/add-sora-auto-reconnect
- Polished: {YYYY-MM-DD}

## 目的

momo (C++) は Sora モードで watchdog ベースの自動再接続を持つが、momo-rs は接続が終わるとプロセスも終了する。通信断や Sora の再起動のあとに、人手の再開操作なしで配信を戻せるようにする。

## 現状

- `src/sora.rs` の `run()` は `SoraConnection::builder()` で作った client を 1 回 `run()` するだけで、戻るとキャプチャリソースを破棄して終了する
- sora_sdk 2026.2.0-canary.3 に再接続の API はなく、`SoraConnectionHandle` が持つのは `selected_signaling_url` / `connected_signaling_url` / `disconnect` / `send_rpc_request` / `send_message` / `get_stats` のみ
- 切断の検知自体は `src/sora/event_handler.rs` の `on_websocket_close` で受けられる
- `docs/MOMO.md` の「自動再接続」に momo の仕様 (5, 15, 25, ... 秒間隔で上限なし) を記載済み
- `issues/0001` の解決方法に「sora_sdk に再接続機能があるか要確認」として書かれていたが、接続経路の実装と実装単位が異なるため切り出した

## 設計方針

- 再接続は sora_sdk の内側ではなく momo-rs 側で `run()` をラップして行う。sora_sdk は 1 接続 = 1 `SoraConnection` のため、再接続は新しい client とイベントハンドラの生成まで含む
- `SoraConnectionContext` と音声・映像トラックを再接続ごとに作り直せるかを確認する。デバイスを開き直す方案は失敗パターンを増やしやすいため避ける
- 再接続間隔は momo に合わせ 5 秒刻みで増加させ (5, 15, 25, ...)、接続に成功したらリセットする
- `--ignore-disconnect-websocket` 有効時は WebSocket が閉じても DataChannel で通信が続くため、再接続を発火させない

## 完了条件

- Sora との接続が切断されると、間隔を伸ばしながら再接続が試みられ、成功後は同じチャネルで配信が再開される
- 手動でシグナリングサーバーを落としても、復帰後に配信が自動で戻ることを E2E テストで確認できる
- `docs/MOMO.md` の「未実装」記載が解消される

## 実装状況

- 再接続ループは実装済み (`src/sora.rs` の `run` / `connect`)
- 再接続間隔は momo の `SoraClient::ReconnectAfter` と同じ `5 * (2 * retry + 1)` 秒。接続を確立したうえでの終了では回数を 0 に戻す。momo は `SoraClient::DoIceConnectionStateChange` で ICE 接続状態が connected になったときにリセットするが、sora_sdk は ICE 状態をイベントとして公開しないため、接続確立後にしか発生しないイベント (`on_notify` / `on_push` / `on_track` / `on_switched` / `on_websocket_close`) の受信で代用する
- 接続ごとに `SoraConnectionContext`・AudioDeviceModule・キャプチャ・トラックを作り直す。sora_sdk が `AdmConfig::UseExternal` で AudioDeviceModule の所有を取るため、接続をまたいで共用できない
- E2E は `e2e-tests/test_sora_reconnect.py` に認証情報なしで動く部分を実装した。実 Sora での配信復旧は未確認

### 残作業 (要上流対応)

momo の watchdog に相当する「シグナリングが静かなら能動切断する」監視は未実装。実装できないことが確定しているため、安易に載せない。

- momo は Sora から届く `{"type": "ping"}` を受信したときに `watchdog_.Reset()` している。接続前は 30 秒、ICE connected 後は 60 秒、DataChannel シグナリング切替後は `data_channel_signaling_timeout` 秒を期限に切り替える (`SoraClient::DoIceConnectionStateChange`)
- sora_sdk 2026.2.0-canary.3 は受信 ping を内部で処理して pong を返すだけで、`SoraConnectionEventHandler` には通知しない。`on_signaling_message` もメッセージ種別ごとにしか呼ばれない (ping 経由では呼ばれない)
- そのため「ハンドラが受け取った最後のイベントから N 秒で切る」を実装すると、映像だけ受けていてシグナリングが静かなチャネルで 60 秒ごとに誤再接続する
- WebSocket 経路の片半開検知は、sora_sdk が使う shiguredo_websocket の ping (10 秒間隔) + pong タイムアウトが担っている

上流へは「受信した全シグナリングメッセージ (ping を含む) をハンドラへ通知する」要望を出す。それが入れば #0025 も同じ機構で実装できる。
