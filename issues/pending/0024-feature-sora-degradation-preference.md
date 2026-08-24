# Sora モードで --degradation-preference を反映する

- Created: 2026-09-07
- Completed: {YYYY-MM-DD}
- Branch: feature/add-sora-degradation-preference
- Polished: {YYYY-MM-DD}

## 目的

P2P / Ayame では `--degradation-preference` が映像送信に反映されるが、Sora モードでは指定時点で起動に失敗する。Sora でも負荷時の品質制御の方針を選べるようにする。

## 現状

- `src/main.rs` の `run_sora` は `momo_config.degradation_preference` が指定されると「sora_sdk に degradation preference API がありません」で終了する
- sora_sdk 2026.2.0-canary.3 の `SoraConnectionBuilder` / `SoraConnectionContextConfig` に degradation preference の設定口はない
- 上流の sora-rust-sdk では issue 0151「DegradationPreference を設定できるようにする」が open で対応中。対応コミットは 2026.2.0-canary.3 のバージョンバンプより後にあるため、まだ公開版には含まれない
- CLI の統一 (`--priority` / `--fixed-resolution` の廃止) は完了済み。Sora への反映は上流 API 依存のため切り出した

## 設計方針

- 上流 API が公開され次第、momo-rs の `balanced` / `maintain-framerate` / `maintain-resolution` を上流の列挙型へ変換して接続設定に渡す
- 上流は sender の `RtpParameters` に設定する方式を想定している (既存の `--degradation-preference` と同じ適用点になる)
- `maintain-framerate-and-resolution` は上流でも未対応のため、対応が入るまでは現状どおり明示エラーを維持する (`issues/0022`)

## 完了条件

- Sora モードで `--degradation-preference` を指定しても起動に失敗せず、設定が送信側の映像品質制御に反映される
- `docs/MOMO.md` の Sora モード「未対応」記載が解消される

## pending にする理由

sora_sdk に degradation preference の設定口がなく、momo-rs 側で着手できる作業が無いため。上流の sora-rust-sdk issue 0151 で対応が進行中なので、設定口を含む版が公開された時点で reopened して対応する。
