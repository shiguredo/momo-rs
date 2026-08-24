# lib クレート分離して tests / pbt / fuzz を置ける構造にする

- Created: 2026-09-07
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-split-lib-and-bin
- Polished: {YYYY-MM-DD}

## 目的

shiguredo-rust 規約が求める単体テスト (`tests/`)・PBT (`pbt/`)・Fuzzing (`fuzz/`) は公開 API に対して書く。momo-rs は bin クレート単体で公開 API が無いため、これらの置き場所が存在しない。ライブラリクレートを分離してテスト書ける構造にする。

## 現状

- `Cargo.toml` は `[[bin]] name = "momo"` だけで `[lib]` を持たない。`src/main.rs` が全モジュールを `mod` で抱えている
- 結果として `cargo test --workspace` は bin の `#[cfg(test)]` しか実行できず、現在の単体テスト 8 件も `src/ayame/mod.rs` と `src/sora/connect_options.rs` の内側に置いている (`tests/` に移せない)
- Makefile の `pbt-with-cover` は `-p pbt` を、`fuzzing` / `fuzzing-parallel` / `fuzzing-list` は `cargo fuzz list` を参照するが、`pbt/` と `fuzz/` は存在せず実行できない
- 公開 API が無いため、Fuzzing の対象にしたい JSON 解析 (受信する `forwarding_filters` の解析や `metadata` の JSON 変換) が内部関数に留まっている

## 設計方針

- `src/lib.rs` を追加し、モード実装 (`ayame` / `p2p` / `sora`)・設定型・エラー型を `pub` で公開する
- `src/main.rs` は CLI の組み立てに専念する。noargs による引数定義、`--log-level` の tracing 初期化、モードの振り分けは bin 側に残す
- ライブラリから CLI 型 (`noargs::Error` など) を外部へ漏らさない。設定検証は `sora::connect_options` のように純粋な関数として公開し、CLI 側がエラー表示へ変換する
- `include` は `["/LICENSE", "/README.md", "/src/**"]` のままで、lib 追加の影響は無い
- MSRV (`rust-version = "1.98"`) は変更しない。feature 構成は `player` / `raspberrypi` / HW エンコーダー (`nvcodec` / `vpl` / `amf`) で、`ayame` / `sora` は常時有効

## 完了条件

- `cargo build` / `cargo test --workspace` / `cargo clippy --workspace -- -D warnings` が従来どおり通る
- `tests/test_<module>.rs` に単体テストを置ける。`connect_options` の検証群を `tests/` へ移す
- `pbt/` で noprop による PBT が動く (`make pbt-with-cover` が成功する)
- `fuzz/` に JSON 解析のターゲットが置け、`make fuzzing-list` が空にならない

## 関連

- shiguredo-rust 規約のテスト役割分担 (PBT は公開 API 対象、`pbt/` と `fuzz/` は `src/` に書かない)
