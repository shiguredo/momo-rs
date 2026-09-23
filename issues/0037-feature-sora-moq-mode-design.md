# sora-moq モードの引数を確定して CLI に反映する

- Created: 2026-09-16
- Completed: 2026-09-23
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

momo-rs に MOQT (Media over QUIC Transport) のクライアントモードを追加する。MOQT は publisher と subscriber で操作もメディア経路も非対称なため、引数体系を確定して CLI に実装し、publisher と subscriber の実装に着手できる状態にする。

## 現状

- `moq` feature と `src/moq/` (CLI、URL パース、QUIC 接続、セッション確立) を実装済み
- `shiguredo_moqt` は crates.io 未公開のため git のリビジョンを固定して参照する
- MOQT セッションの確立 (SETUP の交換) まで動作し、トラックの publish / subscribe は未実装
- 引数は `--url` と `--ca-cert` のみ。メディア関連と `--namespace` の引数は未定義
- README の Sora MOQT モード節は `--namespace` を必須として記載しているが、CLI には未実装 (`--namespace` を付けて実行すると `unexpected argument` になる)
- TLS の扱いが既存モードと揃っていない。`--ca-cert` は独自名で、既存の `--cacert` は PEM の内容、`--insecure` は検証スキップを意味する
- 現行の `src/moq/quic.rs` は CA 証明書が未指定のとき常に検証をスキップする (開発用の暫定実装)
- `src/main.rs` の `run_sora_moq` は `MomoConfig` を受け取らないため、既存のグローバルオプションが moq モードに届かない
- CI は `moq` feature を検証していない。`--features ayame,sora` を指定しているジョブは feature が存在せず失敗する

## 設計方針

### コマンド体系

```
momo sora-moq publish
momo sora-moq subscribe
```

`momo p2p` / `momo ayame` / `momo sora` と同じ階層に `sora-moq` を置き、その配下で役割を選ぶ。

publisher と subscriber を別モードにしない理由は、MOQT の publisher と subscriber が同一モードの方向違いではなく、使う操作 (PUBLISH と SUBSCRIBE / FETCH) もメディア経路 (エンコードとデコード) も異なるため。WebRTC の `--role` のように 1 つのモードの修飾として扱えない。

### 引数

| 用途 | 引数 | 出典 |
|---|---|---|
| 接続先 | `--url` (必須) | MOQT §6.1 (MOQT URI Scheme) |
| Track Namespace | `--namespace` (必須) | MOQT §2.4.1 (Track Naming) / MSF §5.2.2 |
| Track 名 | `video` / `audio` 固定 | MSF §5.2.3 |
| 映像ビットレート | `--video-bit-rate` (必須) | MSF §5.2.22 |
| 音声ビットレート | `--audio-bit-rate` (必須) | MSF §5.2.22 |
| キーフレーム間隔 (フレーム数) | `--video-keyframe-interval` (任意) | MSF §5.2.24 (Maximum GOP Duration) |
| 解像度 | `--resolution` (既存、任意) | MSF §5.2.26 / §5.2.27 |
| フレームレート | `--framerate` (既存、任意) | MSF §5.2.20 |
| TLS | `--insecure` / `--cacert` (既存) | - |
| トラックの有効化 | `--no-video-input-device` / `--no-audio-device` (既存) | - |
| デバイス | 既存のデバイス系オプション | - |

- Track 名は `video` と `audio` の固定にする。momo には映像と音声以外の入力経路がないため、引数で変えられる必要がない
- subscribe では Track 名を指定させない。publisher が固定名で publish するため購読側はカタログから引ける。引数にすると publisher と食い違う余地ができる
- 削除する引数: `--ca-cert` (既存の `--cacert` に統合する)
- 追加しない引数: `--use-datagram` (配信経路の選択は後続で判断する)

#### ビットレート引数の置き場所

`--video-bit-rate` と `--audio-bit-rate` は現在 `run_sora` の中でのみ宣言している。p2p / ayame では使えないため共通化せず、`run_sora_moq` の中でも宣言する。トップレベル (`MomoConfig`) へは移さない。

`run_sora_moq` 側では既定値を置かず、`.present()` で指定の有無を取り出す。`run_sora` は `.default("0")` を維持し、0 を「未指定」として扱う既存の挙動を変えない。

`run_sora_moq` で既定値を置かないのは、`noargs` の `default` では「利用者が 0 を指定した」と「既定値の 0」を区別できないため。有効なトラックに対応する引数が指定されていなければ起動時にエラーにし、指定された値が範囲外 (0 を含む) でもエラーにする。

`--video-keyframe-interval` も `run_sora_moq` の中でのみ宣言する。Sora モードはキーフレーム間隔を指定する引数を持たず、追加すると引数集合が変わるため。

`run_sora_moq` は `--resolution` / `--framerate` / `--cacert` / `--insecure` を `MomoConfig` から受け取るため、`run_sora_moq` と `moq::cli::run` のシグネチャを変更する。

### トラックの有効化

既存モードの引数の意味と、sora-moq での扱いを次に定める。

| 引数 | 既存モード (sora) の意味 | sora-moq での意味 |
|---|---|---|
| `--video` / `--audio` | トラックを作るかどうか | 使用しない (指定されても無視する。`--help` の説明は変えない) |
| `--no-video-input-device` | 映像トラックを作らない (`AdaptedVideoTrackSource` を生成しない) | 同じ (映像トラックを publish しない) |
| `--no-audio-device` | 音声トラックは作り、`AdmConfig::NoAudioDevice` により無音を送る | **変える** (音声トラックを publish しない) |

sora-moq には入力デバイスを差し替える経路がないため、両方の引数を「トラックを publish しない」の意味に統一する。この差は意図的であり、sora モードと同じ「無音トラックを publish する」挙動にはしない。

`--no-video-input-device` と `--no-audio-device` を同時に指定するとカタログに載せるトラックが 0 本になるため、起動時にエラーにする。トラックが 1 本もないカタログを publish しても意味がないため。

### 購読側のトラック識別

カタログの `role` は MSF §5.2.6 で Optional のため、識別に使わない。**購読側はカタログの `codec` の接頭辞で映像と音声を判定する**。

| 種別 | 判定 |
|---|---|
| 映像 | `codec` が `avc1` / `avc3` / `hvc1` / `hev1` / `av01` のいずれかで始まる |
| 音声 | `codec` が `opus` で始まる |

publish 側は `role` と `codec` の両方をカタログに載せる (`role` は MSF §5.2.6 の許容値 `video` / `audio`)。`role` を載せない publisher のカタログも購読できるようにするため、識別は `codec` に統一する。

`codec` は MSF §5.2.18 で音声と映像のトラックに MUST であり、値は RFC 6381 形式にする。映像は H.264 の `avc1.PROFILELEVEL` (例: `avc1.42E01E`) または `avc3` を使う。どちらを使うかは payload 形式 (LOC §2.1.3 の長プレフィックス / §2.1.4 の start code) に依存するため後続で決める。

subscribe で購読するトラックの選択は、`--video-track` / `--audio-track` のような引数を設けず、両方を購読する。片方だけ購読したい場合はカタログに存在しないトラックを購読しない実装で対応する。この判断は後続で必要になれば見直す。

### カタログに載せる項目

publish 側が生成する MSF カタログに必ず載せる項目を確定する。

root catalog は `shiguredo_moqt::msf` の `MsfCatalog` を使い、`version` などの root 項目はライブラリが設定する (`MSF_VERSION` の値)。`tracks` はここで確定する項目から生成する。

本 issue では載せる項目と値の導出方法の確定までを行い、カタログの生成と `catalog` トラックの publish はメディア経路の issue で実装する。

| 項目 | 映像 | 音声 | 出典 |
|---|---|---|---|
| `name` | `video` | `audio` | MSF §5.2.3 |
| `namespace` | `--namespace` の値 | 同左 | MSF §5.2.2 |
| `packaging` | `loc` | `loc` | MSF §5.2.4 |
| `isLive` | `true` | `true` | MSF §5.2.7 |
| `role` | `video` | `audio` | MSF §5.2.6 |
| `codec` | sample entry 名で始まる形式 (`avc1.PROFILELEVEL` など) | `opus` | MSF §5.2.18 |
| `bitrate` | `--video-bit-rate` を 1000 倍 | `--audio-bit-rate` を 1000 倍 | MSF §5.2.22 |
| `width` / `height` | `--resolution` の値 | - | MSF §5.2.26 / §5.2.27 |
| `framerate` | `--framerate` の値 | - | MSF §5.2.20 |
| `timescale` | 90000 | 48000 | MSF §5.2.21 / LOC §2.3.1.2 |
| `samplerate` | - | 48000 | MSF §5.2.28 |
| `channelConfig` | - | `"2"` (ステレオ固定。MSF §5.2.29 の例に合わせる) | MSF §5.2.29 |
| `maxGopDuration` | `--video-keyframe-interval` × 1000 ÷ `--framerate` (ミリ秒) | - | MSF §5.2.24 |

`maxGopDuration` はミリ秒のため、フレーム数の引数から換算する。整数演算で先に除算すると精度が落ちるため「フレーム数 × 1000 ÷ フレームレート」の順で計算する。既定値では 60 × 1000 ÷ 30 = 2000 ミリ秒になる。

`--video-keyframe-interval` はエンコーダーに渡すキーフレーム間隔として使い、実際の GOP 長がこの値に従うようにする。カタログの `maxGopDuration` はその換算値とする。

無効化されたトラック (`--no-video-input-device` / `--no-audio-device`) はカタログに載せない。MSF §5.2.22 の MUST は音声と映像のトラックに対する要求なので、載せなければ対象外になる。

### 仕様から決まる制約

- カタログ track の名前は `catalog` 固定 (MSF §5)
- MSF の `bitrate` は bits per second。既存の `--video-bit-rate` は kbps のため 1000 倍して格納する
- MSF §5.2.22 の `bitrate` は最大ビットレート、`--video-bit-rate` はエンコーダーの目標ビットレートのため、目標値をそのまま最大値として広告する。マージンを持たせる判断は publisher 実装時に行う
- `--namespace` はカタログ track と映像・音声トラックの両方に適用する (MSF §5.2.2 の継承に依存しない)

### TLS の統合

`--ca-cert` を削除し、既存の `--cacert` と `--insecure` に寄せる。既存モードとの意味の差を次のとおり揃える。

| 指定 | 挙動 |
|---|---|
| `--cacert PATH` | 指定した CA 証明書で検証する。`MomoConfig.ca_cert` は既存モードと同じく PEM の内容を保持するため、`src/moq/quic.rs` のファイル読込みをやめて PEM の内容を受け取る形にする |
| `--insecure` | 証明書検証をスキップする。`--cacert` の指定より優先する |
| どちらも未指定 | システムのルート証明書で検証する |

優先順位は Ayame の `src/ayame/signaling.rs` の `create_tls_config` と同じにする。現行実装は「CA 証明書が未指定なら常に検証をスキップする」ため、未指定時は検証するように既定を反転させる。

### CI

CI は `moq` feature を検証していないため、引数の実装を CI で守れない。`cargo clippy` / `cargo build` / `cargo test` に `--features moq` を加えたジョブを追加する。feature が存在しない `--features ayame,sora` の指定も解消する。

## 完了条件

- `momo sora-moq publish` と `momo sora-moq subscribe` の引数が上記のとおり定義され、`--help` に表示される
- 未指定時に `--resolution` が VGA、`--framerate` が 30、`--video-keyframe-interval` が 60 になる
- 必須引数の欠落が起動時にエラーになる。`--url` / `--namespace` は両サブコマンドで、`--video-bit-rate` / `--audio-bit-rate` は対応するトラックが有効なときに限る
- sora-moq で `--video-bit-rate 0` と `--audio-bit-rate 0`、および範囲外の値 (映像 30001 以上、音声 511 以上) が起動時にエラーになる
- `--video-keyframe-interval` が 1 未満の値で起動時にエラーになる
- `--no-video-input-device` と `--no-audio-device` の同時指定が起動時にエラーになる
- `--ca-cert` が削除され、`--cacert` と `--insecure` に統合される。`--cacert` 未指定かつ `--insecure` 未指定のときに検証が有効になり、両方指定時は `--insecure` が優先される
- `--video-bit-rate` と `--audio-bit-rate` が `run_sora` と `run_sora_moq` の両方で指定でき、Sora モードの挙動が変わらない
- 引数の検証を `src/moq/cli.rs` の `#[cfg(test)]` モジュールの単体テストで担保する
- README の Sora MOQT モード節と `docs/MOMO.md` の Sora モードのオプション記載が実装に合う
- CI の `moq` feature のジョブで clippy / build / test が実行され、`--features ayame,sora` の指定が解消される
- `cargo test --workspace --features moq` と `cargo clippy --workspace --all-targets --features moq -- -D warnings` が通る

## 解決方法

- `momo sora-moq publish` / `subscribe` の引数 (`--url` / `--namespace` / `--video-bit-rate` / `--audio-bit-rate` / `--video-keyframe-interval`) を実装した
- `--no-video-input-device` / `--no-audio-device` で publish するトラックを選び、同時指定は起動時にエラーにした
- TLS を `--cacert` と `--insecure` に統合し、未指定時はシステムのルート証明書で検証するようにした (`--insecure` が `--cacert` より優先)
- `run_sora_moq` が `MomoConfig` から解像度 / フレームレート / TLS / デバイス指定を受け取るようにした
- 引数の検証は `src/moq/config.rs` の `MoqConfig::validate` と `src/moq/cli.rs` の単体テストで担保した
- CI の macOS ジョブが `moq` feature を検証するようにした
- ローカルの sora-moq 開発 relay で publish / subscribe の動作を確認した

## 関連

- `docs/MOMO.md`: momo との機能比較。MOQT は momo に対応機能がないため、Sora モードのオプション記載の更新のみ行う
- メディア経路 (payload 形式、SPS/PPS の抽出、Timescale) は別 issue で扱う
