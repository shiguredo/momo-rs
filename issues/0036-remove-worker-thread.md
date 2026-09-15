# libwebrtc の worker_thread 削除に追随する

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-worker-thread
- Polished: {YYYY-MM-DD}

## 目的

libwebrtc の issue 558821261「Deprecate and remove PeerConnectionFactoryDependencies::worker_thread」が実装されて `PeerConnectionFactoryDependencies::worker_thread` と `PeerConnectionFactoryInterface::worker_thread()` が削除された後も momo-rs をビルドできるようにする。`worker_thread` の利用箇所と API を全て無くすことで、この削除に追随する。

## 現状

- `src/ayame/webrtc.rs` の `AyameEngine::new` と `src/p2p/webrtc.rs` の `WebRtcEngine::new` が `PeerConnectionFactoryDependencies::set_worker_thread` を呼んでいる。方針 1（専用 worker thread をやめて network thread を使う）の対応後も、network thread を渡すこの行は残る
- 依存は `shiguredo_webrtc = "0.152.1-canary.2"` だが、`sora_sdk 2026.2.0-canary.3` が `shiguredo_webrtc ~0.152.1-canary.2` を要求するため、`sora_sdk` が新しい `shiguredo_webrtc` を許容する版を出さない限り上げられない。この理由は `Cargo.toml` のコメントに書かれている
- libwebrtc の削除系 CL（499302 / 501640 / 501720 / 502000 / 502500 / 502860 / 502940 / 502960）はレビュー中で、`PeerConnectionFactoryDependencies::worker_thread` と `PeerConnectionFactoryInterface::worker_thread()` は将来削除される
- 時雨堂の方針は 2 段階で、本 issue は方針 2（558821261 を実装した libwebrtc をマージした後に worker_thread の利用箇所と API を全て無くす）に対応する。方針 1 は先行 issue で対応する
- 本 issue の前提条件は、webrtc-rs が 558821261 を実装した libwebrtc に追随した `shiguredo_webrtc` をリリースし、かつ `sora_sdk` がそれを許容する版をリリースしていることである。現時点では満たされていないため、前提が満たされるまで着手できない

## 設計方針

- `shiguredo_webrtc` と `sora_sdk` の依存を、558821261 を実装した libwebrtc に追随したバージョンへ更新する
- `src/ayame/webrtc.rs` の `AyameEngine::new` と `src/p2p/webrtc.rs` の `WebRtcEngine::new` から `set_worker_thread` の呼び出しを削除する
- 方針 1 の対応で `worker_thread` を保持するフィールドと worker 用の `Thread` の生成は既に削除されているため、本 issue では残っている `set_worker_thread` の呼び出しを削除する

## 解決方法

- `Cargo.toml` の `shiguredo_webrtc` と `sora_sdk` を、前提条件を満たすバージョンに更新する
- `AyameEngine::new` と `WebRtcEngine::new` から `deps.set_worker_thread(...)` を削除する

## 完了条件

- `set_worker_thread` と worker thread 関連の参照が 0 件であること
- `shiguredo_webrtc` と `sora_sdk` の依存が更新されていること
- `cargo check` / clippy / test が通ること
- `CHANGES.md` に追記すること
