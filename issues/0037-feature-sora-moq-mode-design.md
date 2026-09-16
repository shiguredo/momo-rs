# sora-moq モードの引数を確定して基盤を実装する

- Created: 2026-09-16
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moq-mode
- Polished: {YYYY-MM-DD}

## 目的

momo-rs に MOQT (Media over QUIC Transport) のクライアントモードを追加する。MOQT は publisher と subscriber で操作もメディア経路も非対称なため、引数体系を先に確定させてから publisher と subscriber の実装に着手できる状態にする。

## 現状

- `moq` feature と `src/moq/` (CLI、URL パース、QUIC 接続、セッション確立) を実装済み
- `shiguredo_moqt` は crates.io 未公開のため git のリビジョンを固定して参照する
- MOQT セッションの確立 (SETUP の交換) まで動作し、トラックの publish / subscribe は未実装
- 引数は `--url` と `--ca-cert` のみで、メディア関連の引数は未定義
- TLS の扱いが既存モードと揃っていない (`--ca-cert` は独自名で、既存の `--cacert` と `--insecure` がある)

## 設計方針

### コマンド体系

```
momo sora-moq publish
momo sora-moq subscribe
```

`momo p2p` / `momo ayame` / `momo sora` と同じ階層に `sora-moq` を置き、その配下で役割を選ぶ。

publisher と subscriber を別モードにしない理由は、MOQT の publisher と subscriber が同一モードの方向違いではなく、使う操作 (PUBLISH と SUBSCRIBE / FETCH) もメディア経路 (エンコードとデコード) も異なるため。WebRTC の `--role` のように 1 つのモードの修飾として扱えない。

### 引数

既存のグローバルオプションで足りるものは追加しない。

| 用途 | 引数 | 出典 |
|---|---|---|
| 接続先 | `--url` (必須) | MOQT §6.2 |
| Track Namespace | `--namespace` (必須) | MOQT §8.7 |
| Track 名 | `video` / `audio` 固定 | MSF §5.2.3 |
| 映像ビットレート | `--video-bit-rate` (必須) | MSF §5.2.22 |
| 音声ビットレート | `--audio-bit-rate` (必須) | MSF §5.2.22 |
| 解像度 | `--resolution` (既存) | MSF §5.2.26 / §5.2.27 |
| フレームレート | `--framerate` (既存) | MSF §5.2.20 |
| キーフレーム間隔 | `--video-keyframe-interval` | LOC §2.1 |
| TLS | `--insecure` / `--cacert` (既存) | - |
| デバイス | 既存のデバイス系オプション | - |

- Track 名は `video` と `audio` の固定にする。momo には映像と音声以外の入力経路がないため、引数で変えられる必要がない
- subscribe では Track 名を指定させない。MSF のカタログは `name` と `role` を含むため購読側はカタログから引ける。引数にすると publisher と食い違う余地ができる
- 削除する引数: `--ca-cert` (既存の `--cacert` に統合する)
- 追加しない引数: `--no-video` / `--no-audio` (既存の `--no-video-input-device` / `--no-audio-device` と同じ意味)、`--use-datagram` (配信経路の選択は後続で判断する)

### 必須引数の基準

利用者しか知らない値は必須にし、安全な既定を置ける値は既定を持たせる。

- 必須: `--url`、`--namespace`、`--video-bit-rate`、`--audio-bit-rate` (有効なメディアのものだけ)
- 任意: `--resolution` (VGA)、`--framerate` (30)、`--video-keyframe-interval` (60)

ビットレートを必須にする理由は、MSF §5.2.22 が音声と映像のトラックに `bitrate` を MUST と定めており「未指定」を表現できないため。Sora モードの `0` は「SDK に任せる」の意味だが、MOQT モードには任せ先がない。

`--video-bit-rate` と `--audio-bit-rate` は現在 sora モード専用のため、トップレベルへ移して `MomoConfig` に載せる。既定は Sora モードの挙動を変えないよう `0` のままにし、sora-moq 側で `0` を拒否する (有効範囲は映像 1-30000、音声 1-510)。

### 仕様から決まる制約

- カタログ track の名前は `catalog` 固定 (MSF §5.3)
- MSF の `bitrate` は bits per second。既存の `--video-bit-rate` は kbps のため 1000 倍して格納する
- 映像の payload は 4 バイト長プレフィックス (canonical) と start code (annexB) のどちらかに統一する (LOC §2.1.3 / §2.1.4)
- SPS/PPS は Video Config (0x0D) に載せる (LOC §2.3.2.1)
- Timestamp の単位は Timescale で定める (映像 90000、音声 48000 など。LOC §2.3.1.1 / §2.3.1.2)

## 完了条件

- `momo sora-moq publish` と `momo sora-moq subscribe` の引数が上記のとおり定義され、`--help` に表示される
- 必須引数の欠落と範囲外の値が起動時にエラーになる
- `--ca-cert` が削除され `--cacert` と `--insecure` に統合される
- `--video-bit-rate` と `--audio-bit-rate` がトップレベルに移り、Sora モードの挙動が変わらない
- `cargo test --workspace --features moq` と `cargo clippy --workspace --all-targets --features moq -- -D warnings` が通る

## 実装状況

- CLI の骨格 (`momo sora-moq publish` / `momo sora-moq subscribe`) とセッション確立 (SETUP の交換) は実装済み
- 引数の体系は上記のとおり確定
- トラックの publish と subscribe、メディアの送受信は未実装

## 関連

- `docs/MOMO.md`: momo との機能比較。MOQT は momo に対応機能がないため記載方法を別途判断する
