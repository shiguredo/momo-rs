# sora-moq のメディア経路を実装する

- Created: 2026-09-16
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

momo-rs の `momo sora-moq publish` / `momo sora-moq subscribe` で、MOQT のトラックとして映像と音声を送受信できるようにする。MOQT セッションの確立 (SETUP の交換) は実装済みで、引数の体系は `issues/0037-feature-sora-moq-mode-design.md` で確定する。本 issue は引数の実装完了を前提とし、メディアの符号化、パッケージング、復号、再生を扱う。

## 現状

- MOQT セッションの確立 (SETUP の交換) まで実装済み。トラックの publish と subscribe は未実装
- `src/openh264.rs` の H.264 エンコーダーは SPS/PPS を 4 バイトのスタートコードで前置した Annex B 形式を生成する。SPS/PPS が付くのは SPS を含むフレーム (通常は IDR フレーム) に限られる
- 音声のエンコード経路がない。momo の Sora モードは libwebrtc のエンコーダーに委ねているため、`shiguredo_opus` などを用いた独自の経路が必要
- カタログに載せる項目と値の導出方法は `issues/0037-feature-sora-moq-mode-design.md` で確定している。映像の `codec` は sample entry 名で始まる形式とし、payload 形式に応じて `avc1` と `avc3` のどちらを使うかを本 issue で決める
- `shiguredo_moqt` は LOC と MSF の codec を提供する。`shiguredo_moqt::loc` のプロパティと `shiguredo_moqt::msf` のカタログを使う

## 設計方針

### カタログの生成と publish

`issues/0037-feature-sora-moq-mode-design.md` で確定した項目を持つ MSF カタログを生成し、`catalog` トラックとして publish する。`shiguredo_moqt::msf` の `MsfCatalog` を使い、root 項目はライブラリに設定させる。

カタログの内容は映像と音声のトラック構成と一致させる。`codec`、`timescale`、`channelConfig` など、実際の符号化の設定と対応する値は、符号化側の設定から導出する。

### 映像のパッケージング

LOC は映像 payload の形式として、4 バイトの長プレフィックス (canonical、LOC §2.1.3) と 4 バイトのスタートコード (annexB、LOC §2.1.4) の両方を許容する。`src/openh264.rs` の出力は Annex B のため、次のいずれかを選ぶ。

- Annex B のまま送る (変換が不要。パラメーターセットは payload に含まれる)
- 長プレフィックス形式に変換して送る (パラメーターセットは Video Config に分離する)

選択した形式に合わせて、カタログの `codec` を `avc1` 系 (パラメーターセットを Video Config に置く) と `avc3` 系 (payload に含める) のどちらにするかを決める。

`avc1` 系を使う場合の `PROFILELEVEL` は、エンコーダーが出力する SPS から `profile_idc` / `constraint_flags` / `level_idc` を取り出して組み立てる。固定値は使わない。

### パラメーターセットの扱い

SPS/PPS を payload に含めるか Video Config (0x0D) に載せるかは、映像のパッケージング形式と揃える。LOC §2.1.1 は payload 側、§2.1.2 はヘッダー側を許容する。

Video Config に載せる場合は、`src/openh264.rs` が出力する Annex B から SPS/PPS を抽出する処理が必要になる。抽出した値は LOC の Video Config プロパティにそのまま設定する。

### タイムスタンプ

LOC §2.3.1.2 の Timescale で Timestamp の単位を定める。同節は 1000000 (マイクロ秒)、48000 (音声)、90000 (映像) を例示するのみで要求ではないため、映像は 90000、音声は 48000 を使うという本 issue の設計判断とする。

エンコーダーやキャプチャが返す時刻 (マイクロ秒など) から、Timescale に合わせた値へ変換する。変換の丸めで同じ Timestamp が続かないようにする。

### 音声のコーデック

音声は Opus を使い、Audio Config (0x0F) に OpusHead (RFC 7845 §5.1) を載せる。サンプリングレートは 48000、チャンネル数は 2 (ステレオ) とし、カタログの `channelConfig: "2"` と一致させる。

### トラックと object の構成

- track 名は `video` と `audio`、namespace は `--namespace` の値を使う
- Subgroup と Datagram のどちらで送るかを決める。既定は Subgroup とし、Datagram は後続で判断する
- キーフレームの位置で Group を区切る。GOP 内のピクチャーを同じ MOQT Group に置く要求は MSF §4.1、GroupID を IDR フレーム境界で進める記述は LOC §4.2 / §4.3 にある

### 購読と再生

- カタログを FETCH して映像と音声のトラックを見つける。識別は `codec` の接頭辞で行う
- 映像は `shiguredo_openh264::Decoder` で復号する。Sora モードが使う `shiguredo_webrtc::VideoDecoder` の経路は libwebrtc のトラックを前提とするため流用できない。表示は `player` feature の `raw_player` を使う
- 音声は `shiguredo_audio_device` の再生経路に渡す

## 完了条件

publish 側:

- 映像と音声のトラックを publish し、MSF に準拠したカタログを `catalog` トラックとして publish する
- `issues/0037-feature-sora-moq-mode-design.md` で確定したカタログの項目がすべて設定される
- 映像の payload 形式とカタログの `codec` の組み合わせが一致する
- `codec` の `PROFILELEVEL` がエンコーダーの SPS と一致する
- Video Config と Audio Config が一次資料と一致する。Audio Config は RFC 7845 §5.1 の OpusHead と照合する。Video Config は H.264 の extradata を定める ISO/IEC 14496-15 が `refs/` にないため、`refs/` への追加を検討したうえで照合する
- 実測の GOP 長がカタログの `maxGopDuration` と矛盾しない

subscribe 側:

- カタログからトラックを見つけて購読し、映像を表示して音声を再生する

共通:

- 単体テストで符号化と復号を検証する。E2E テストは MOQT relay を用意できる場合に publisher と subscriber の送受信を検証する (relay の用意ができない場合は単体テストまでとし、その旨を issue に記録する)

## 関連

- MOQT draft-ietf-moq-transport-21 / LOC draft-ietf-moq-loc-04 / MSF draft-ietf-moq-msf-01
- 引数とセッション確立は `issues/0037-feature-sora-moq-mode-design.md` で実装する。本 issue は 0037 の完了後に着手する
- `docs/MOMO.md`: momo との機能比較
