# Ayame / P2P の worker thread を network thread に統一する

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-unify-worker-thread
- Polished: {YYYY-MM-DD}

## 目的

Ayame モードと P2P モードの WebRTC factory のために専用の worker thread を生成するのをやめ、network thread を worker thread として使う。libwebrtc の issue 558821261「Deprecate and remove PeerConnectionFactoryDependencies::worker_thread」で worker thread が廃止される方針が示されており、専用 worker thread を持つ構成は将来の libwebrtc で維持できなくなる。専用 worker thread を先に無くしておくことで、削除系 CL をマージした libwebrtc への追随を `set_worker_thread` の呼び出しを消すだけの作業にする。

## 現状

- `src/ayame/webrtc.rs` の `AyameEngine` と `src/p2p/webrtc.rs` の `WebRtcEngine` に、同じ thread 管理が重複している
- どちらも `Thread::new_with_socket_server()`（network）/ `Thread::new()`（worker）/ `Thread::new()`（signaling）を生成して `start()` し、`PeerConnectionFactoryDependencies` に対して `set_network_thread(&network_thread)` / `set_worker_thread(&worker_thread)` / `set_signaling_thread(&signaling_thread)` を呼んでいる
- 生成した `Thread` は `_network_thread` / `_worker_thread` / `_signaling_thread` としてフィールドに保持している（フィールドは宣言順に drop される）
- worker thread はこの factory 生成以外で使っていない。`blocking_call` / `post_task` / `factory.worker_thread()` の利用は無い
- 依存は `shiguredo_webrtc = "0.152.1-canary.2"`（crates.io）。`PeerConnectionFactoryDependencies::set_network_thread` は既に存在する
- libwebrtc の issue 558821261「Deprecate and remove PeerConnectionFactoryDependencies::worker_thread」で worker thread が廃止される。CL 501620「Default worker thread to network thread」と CL 502480「Warn when a distinct worker thread is configured」はマージ済みで、削除系 CL（499302 / 501640 / 501720 / 502000 / 502500 / 502860 / 502940 / 502960）はレビュー中である
- 時雨堂の方針は 2 段階で、本 issue は方針 1（いますぐ実施。専用 worker thread をやめて network thread を使う）に対応する。方針 2（558821261 を実装した libwebrtc をマージした後に worker_thread の利用箇所と API を全て無くす）は後続 issue で対応する

## 設計方針

- `src/ayame/webrtc.rs` と `src/p2p/webrtc.rs` の両方で、worker 用の `Thread::new()` と `start()` を削除し、`_worker_thread` フィールドを削除する
- 方針 1 では `set_worker_thread` に network thread を渡す。現在の libwebrtc は `worker_thread` が未設定だと内部で専用スレッドを生成するため、`set_worker_thread` の呼び出し自体は残して network thread を渡す。方針 2 の後続 issue でこの行を削除する
- `AyameEngine` / `WebRtcEngine` の drop 順のコメントを実態に合わせて更新する

## 解決方法

- `AyameEngine::new` と `WebRtcEngine::new` から worker thread の生成と `start` を削除する
- 両方の `deps.set_worker_thread(&worker_thread)` を `deps.set_worker_thread(&network_thread)` に変更する
- `AyameEngine` と `WebRtcEngine` から `_worker_thread` フィールドと、`Ok(Self { .. })` の `_worker_thread: worker_thread` を削除する

## 完了条件

- ayame / p2p / sora の各モードでビルドが通り、起動確認ができること
- `cargo check` / clippy / test が通ること
- `CHANGES.md` に追記すること
