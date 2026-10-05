# moqt-rs の rev を 6c3ab63 へ更新し、リセットのエラーコードの型変更に追随する

- Created: 2026-10-05
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

momo-rs の `moq` モードが依存する `shiguredo_moqt` の rev を最新の `develop` へ更新する。
moqt-rs は crates.io 未公開のため git の rev を固定して参照しており、固定している
`a9254ed` は履歴の作り直しにより現行 `develop` の祖先ではなくなった。

## 現状

- `Cargo.toml` は `shiguredo_moqt` を rev `a9254ed13b743b73b9fea992642a1152dce1aca7`
  (2026-09-23) で固定している。この commit は現行 `develop` の最新
  `6c3ab63e89f7c196d269deaee51881dfd9d71730` (2026-10-04) から到達できない
- moqt-rs は `RequestStreamEnd::Reset` の `error_code` を `Option<u64>` に変更している。
  WebTransport では MOQT のコードへ戻せない場合に `None` を使うためである
- `src/moq/transport.rs` の `RecvStream::receive_chunk` と `src/moq/publisher.rs` の
  `SubgroupWriter::finish` が `RequestStreamEnd::Reset` を構築しており、`error_code` に
  `u64` を渡しているため新しい型ではコンパイルできない

## 設計方針

- rev を `6c3ab63` へ更新し、`cargo update -p shiguredo_moqt` で `Cargo.lock` を再生成する
- momo-rs の QUIC 経路は常にアプリケーションエラーコードを持つため `Some(...)` を渡す。
  WebTransport 経路は momo-rs にないため `None` は扱わない
- 実装の挙動は変えない

## 完了条件

- `cargo check --features moq --all-targets` が通ること
- `cargo clippy --features moq --all-targets -- -D warnings` が通ること
- `cargo fmt --all -- --check` が通ること
