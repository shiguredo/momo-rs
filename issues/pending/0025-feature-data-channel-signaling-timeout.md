# --data-channel-signaling-timeout の実装

- Created: 2026-09-07
- Completed: {YYYY-MM-DD}
- Branch: feature/add-data-channel-signaling-timeout
- Polished: {YYYY-MM-DD}

## 目的

momo (C++) と同じ名称・意味で `--data-channel-signaling-timeout` を機能させ、指定した値が実際に効くようにする。

## 現状

- `src/main.rs` の `run_sora` は `--data-channel-signaling-timeout` (デフォルト 180) を `_data_channel_signaling_timeout` としてパースするだけで、値を破棄している
- `docs/MOMO.md` によると momo のこれは DataChannel シグナリングへの切り替え後の watchdog タイムアウト
- sora_sdk 2026.2.0-canary.3 の `SoraConnectionBuilder` に相当する設定口はなく、あるのは `websocket_connection_timeout` / `websocket_close_timeout` / `disconnect_wait_timeout` のみ
- 上流の sora-rust-sdk では pending issue 0007「DataChannel シグナリングタイムアウト」として保留されている
- `docs/MOMO.md` の「パース済みだが未使用」に記載済み

## 設計方針

- momo のこのオプションは、DataChannel シグナリング切替後に watchdog の期限を `data_channel_signaling_timeout` 秒へ切り替えるだけの処理 (`watchdog_.Enable(config_.data_channel_signaling_timeout)`)
- したがって momo-rs 側で実装するなら「最後のシグナリングイベントから指定秒数が経過したら `SoraConnectionHandle::disconnect()` を呼ぶ」形になる。切断手段自体は既存 API で揃っている
- 前提として、受信した全シグナリングメッセージ (ping を含む) をハンドラへ通知する上流 API が必要 (#0023 の残作業参照)。既定値 180 秒は momo と揃える

## 完了条件

- `--data-channel-signaling-timeout` で指定した秒数が DataChannel シグナリングのタイムアウトとして実際に機能する

## pending にする理由

sora_sdk に DataChannel シグナリングのタイムアウト設定口がなく、上流の sora-rust-sdk でも pending issue 0007 として保留されているため。上流で実装されない限り momo-rs 側でできるのは「パースするだけで値が効かない状態」をエラーへ変える対応に留まるが、その場合の選択肢 (オプション廃止 / 明示エラー) は設計判断を要するため保留とする。

## reopened にした理由

pending にした前提 (sora_sdk に設定口が必要) が崩れたため。momo (C++) の `--data-channel-signaling-timeout` は、DataChannel シグナリングへ切り替えた後に `watchdog_.Enable(config_.data_channel_signaling_timeout)` と watchdog の期限を切り替えるだけの処理だった。接続を能動的に閉じる手段は `SoraConnectionHandle::disconnect()` として既存のため、momo-rs 側だけで実装できる。

## pending に戻した理由

reopened にした際の判断が誤りだった。momo の watchdog は Sora から届く `{"type": "ping"}` の受信でリセットされているが、sora_sdk 2026.2.0-canary.3 は受信 ping を `SoraConnectionEventHandler` へ通知しない (`on_signaling_message` も ping 経由では呼ばれない)。通知なしに同等の監視を実装すると、シグナリングが静かなだけで誤って再接続を繰り返す。上流へ通知口を要望するまで着手できない。
