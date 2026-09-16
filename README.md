# WebRTC Native Client Momo (Rust)

[![GitHub tag (latest SemVer)](https://img.shields.io/github/tag/shiguredo/momo-rs.svg)](https://github.com/shiguredo/momo-rs)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

## About Shiguredo's open source software

We will not respond to PRs or issues that have not been discussed on Discord. Also, Discord is only available in Japanese.

Please read <https://github.com/shiguredo/oss> before use.

## 時雨堂のオープンソースソフトウェアについて

利用前に <https://github.com/shiguredo/oss> をお読みください。

## 概要

WebRTC Native Client Momo の Rust 実装です。libwebrtc を利用し、ブラウザなしで動作する WebRTC ネイティブクライアントです。

## モード

### P2P モード

Momo 自体がシグナリングサーバーと HTTP サーバーを持っているため、完全な P2P モードでの利用ができます。
ブラウザから Momo にアクセスするだけで利用できます。

### Ayame モード

WebRTC シグナリングサーバー [Ayame](https://github.com/OpenAyame/ayame) に対応したモードです。

### Sora モード

WebRTC SFU [Sora](https://sora.shiguredo.jp) に対応したモードです。

### Sora MOQT モード

MOQT (Media over QUIC Transport) の relay に接続するモードです。

## 動作環境

| OS | アーキテクチャ | 提供形態 |
|---|---|---|
| macOS 26 | arm64 | ネイティブ |
| macOS 15 | arm64 | ネイティブ |
| Ubuntu 24.04 | x86_64 | ネイティブ |
| Ubuntu 22.04 | x86_64 | ネイティブ |
| Ubuntu 24.04 | arm64 | クロスコンパイル |
| Ubuntu 22.04 | arm64 | クロスコンパイル |

## ビルド

Rust 1.98 以上が必要です（`Cargo.toml` の `rust-version` と合わせています）。

```bash
cargo build --release
```

## クロスコンパイル

Linux 向けのクロスコンパイルはコンテナ内で行います。Apple container を推奨しますが、Docker でも同じ手順が使えます。

### イメージのビルド

```bash
container build -t momo-dev -f .devcontainer/Dockerfile .devcontainer
```

Docker を使う場合は `container` を `docker` に読み替えてください。

### 対応ターゲット

| ターゲット | sysroot 生成 | ビルド |
|---|---|---|
| Raspberry Pi (arm64) | `make sysroot-raspberry-pi` | `make sysroot-build-raspberry-pi` |
| Ubuntu 24.04 (arm64) | `make sysroot-ubuntu-24.04_arm64` | `make sysroot-build-ubuntu-24.04_arm64` |
| Ubuntu 22.04 (arm64) | `make sysroot-ubuntu-22.04_arm64` | `make sysroot-build-ubuntu-22.04_arm64` |

リリースビルドは各ターゲットに `-release` を付けてください（例: `make sysroot-build-raspberry-pi-release`）。

### 手順

1. イメージをビルドする
2. コンテナ内で sysroot を生成してからビルドする

```bash
container run --rm --cpus 8 --memory 16g \
  -v $(pwd):/workspace -v momo-target:/workspace/target -w /workspace \
  momo-dev bash -c 'make sysroot-raspberry-pi && make sysroot-build-raspberry-pi'
```

`target` はボリュームに置きます。Apple container ではマウントしたホストのディレクトリに sysroot を展開できません。

コンテナ内で必要な環境変数の設定例は Makefile の `clippy-raspberry-pi` を参照してください。

## 使い方

### デバイス一覧の取得

利用可能な映像入力・音声入力デバイスを JSON 形式で表示します。

```bash
./momo --list-devices
```

### P2P モード

```bash
./momo p2p --port 8080 --document-root html
```

ブラウザで `http://localhost:8080` にアクセスしてください。

**オプション:**

- `--port PORT`: リッスンポート（デフォルト: 8080）
- `--document-root PATH`: HTTP ドキュメントルートディレクトリ（デフォルト: html）

### Ayame モード

```bash
./momo ayame \
  --signaling-url wss://example.com/signaling \
  --room-id your-room-id
```

**オプション:**

- `--signaling-url URL`: シグナリング URL
- `--room-id ID`: ルーム ID
- `--client-id ID`: クライアント ID（任意）
- `--signaling-key KEY`: シグナリングキー（任意）
- `--direction DIRECTION`: 送受信方向（sendrecv / sendonly / recvonly、デフォルト: sendrecv）

### Sora モード

```bash
./momo sora \
  --signaling-urls wss://example.com/signaling \
  --channel-id your-channel-id \
  --role sendonly
```

**オプション:**

- `--signaling-urls URLS`: シグナリング URL（カンマ区切りで複数指定可）
- `--channel-id ID`: チャネル ID
- `--role ROLE`: ロール（sendonly / recvonly / sendrecv、デフォルト: sendonly）
- `--video BOOL`: 映像送信（デフォルト: true）
- `--audio BOOL`: 音声送信（デフォルト: true）
- `--video-codec-type TYPE`: 映像コーデック（VP8 / VP9 / AV1 / H264 / H265）
- `--audio-codec-type TYPE`: 音声コーデック（OPUS）
- `--video-bit-rate RATE`: 映像ビットレート（デフォルト: 0）
- `--audio-bit-rate RATE`: 音声ビットレート（デフォルト: 0）
- `--spotlight BOOL`: スポットライト（デフォルト: false）
- `--simulcast BOOL`: サイマルキャスト（デフォルト: false）
- `--metadata JSON`: connect メッセージに含めるメタデータ
- `--client-id ID`: Sora に送信するクライアント ID（1〜255 バイト）
- `--bundle-id ID`: Sora に送信するバンドル ID（1〜255 バイト）
- `--signaling-notify-metadata JSON`: シグナリング通知に含めるメタデータ
- `--data-channel-label LABELS`: 接続時に作成するデータチャネル（カンマ区切り、各ラベルは `#` 始まり）
- `--forwarding-filter JSON`: 他参加者の映像・音声の受信を制御する転送フィルター（JSON 配列）

### Sora MOQT モード

MOQT (Media over QUIC Transport) の relay に接続し、トラックを publish または subscribe します。`moq` feature が必要です。

```bash
cargo build --release --features moq
```

```bash
# トラックを publish する
./momo sora-moq publish \
  --url moqt://example.com:4433/live \
  --namespace your-namespace

# トラックを subscribe する
./momo sora-moq subscribe \
  --url moqt://example.com:4433/live \
  --namespace your-namespace
```

**オプション:**

- `--url URL`: MOQT relay の URL（`moqt://host:port/path` 形式。必須）
- `--namespace NAMESPACE`: Track Namespace（必須）

**実装状況:**

MOQT セッションの確立（SETUP の交換）までを実装しています。トラックの publish と subscribe、および映像と音声の送受信は未実装です。

### 受信映像の表示

Sora で受信した映像 (recvonly / sendrecv) を SDL3 ウィンドウにグリッド表示します。ローカルのカメラ映像は表示しません。

**ビルド:**

`player` はデフォルトで有効です。無効にしている場合は明示してください。

```bash
cargo build --release
```

`player` feature はデフォルトで有効です。

**使い方:**

```bash
./momo --player sora \
  --signaling-urls wss://example.com/signaling \
  --channel-id your-channel-id \
  --role recvonly
```

**表示関連オプション:**

- `--player`: 受信映像のウィンドウを表示する（`player` feature が必要）
- `--window-width WIDTH`: ウィンドウ幅（デフォルト: 640）
- `--window-height HEIGHT`: ウィンドウ高さ（デフォルト: 480）

ウィンドウを閉じると表示を終了し、再接続も停止します。

### グローバルオプション

#### デバイス

| オプション | 説明 |
|---|---|
| `--video-input-device DEVICE` | 映像デバイスを名前またはインデックスで指定 |
| `--audio-input-device DEVICE` | 音声入力デバイスを名前、インデックス、unique_id で指定 |
| `--audio-output-device DEVICE` | 音声出力デバイスを名前、インデックス、unique_id で指定 |
| `--no-video-input-device` | 映像入力デバイスを使用しない |
| `--no-audio-device` | 音声デバイスを使用しない |
| `--fake-capture-device` | フェイク映像と音声のキャプチャデバイスを使用する |
| `--list-devices` | 利用可能なデバイス一覧を JSON 形式で出力して終了 |
| `--video-codec-engines` | 利用可能な映像エンコーダーとデコーダーを一覧表示して終了 |
| `--force-i420` / `--force-yuy2` / `--force-nv12` | キャプチャのピクセルフォーマットを強制する |

#### 映像

| オプション | 説明 |
|---|---|
| `--resolution RESOLUTION` | 解像度（QVGA、VGA、HD、FHD、4K、または `WIDTHxHEIGHT`。デフォルト: VGA） |
| `--framerate FRAMERATE` | フレームレート（1-120。デフォルト: 30） |
| `--degradation-preference PREFERENCE` | 劣化時に維持する品質（balanced、maintain-framerate、maintain-resolution） |
| `--window-width WIDTH` | 映像ウィンドウ幅（`player` feature が必要。デフォルト: 640） |
| `--window-height HEIGHT` | 映像ウィンドウ高さ（`player` feature が必要。デフォルト: 480） |
| `--fullscreen` | 映像を全画面表示する（`player` feature が必要） |

#### TLS

| オプション | 説明 |
|---|---|
| `--insecure` | サーバー証明書の検証をスキップする |
| `--cacert PATH` | CA 証明書ファイルを PEM 形式で指定する |
| `--client-cert PATH` / `--client-key PATH` | クライアント証明書と秘密鍵を指定する（同時指定が必要） |

#### ログとメトリクス

| オプション | 説明 |
|---|---|
| `--log-level LEVEL` | ログ出力レベル（verbose、info、warning、error、none。デフォルト: info） |
| `--metrics-port PORT` | メトリクス API のポート番号（デフォルト: -1 で無効） |
| `--metrics-allow-external-ip` | メトリクス API への外部 IP からのアクセスを許可する |

#### コーデック

| オプション | 説明 |
|---|---|
| `--openh264 PATH` | OpenH264 ライブラリのパスを指定する |
| `--h264-encoder TYPE` / `--h264-decoder TYPE` | H.264 のエンコーダーとデコーダーを指定する |
| `--h265-encoder TYPE` / `--h265-decoder TYPE` | H.265 のエンコーダーとデコーダーを指定する |
| `--vp9-encoder TYPE` / `--vp9-decoder TYPE` | VP9 のエンコーダーとデコーダーを指定する |
| `--av1-encoder TYPE` / `--av1-decoder TYPE` | AV1 のエンコーダーとデコーダーを指定する |

#### その他

| オプション | 説明 |
|---|---|
| `--no-google-stun` | Google STUN サーバーを使用しない |
| `--serial DEVICE,BAUDRATE` | シリアルポートとデータチャネルを連携する（Linux のみ） |

## momo (C++ 版) との違い

詳細は [docs/MOMO.md](docs/MOMO.md) を参照してください。

### momo-rs 独自機能

| 機能 | 説明 |
|---|---|
| `--cacert` | CA 証明書ファイルを PEM 形式で指定。momo にはないオプション |

### 仕様の違い

| 機能 | momo | momo-rs |
|---|---|---|
| `--libcamera-control` の形式 | `KEY VALUE` (2 引数) | `KEY=VALUE` (1 引数) |
| プロキシ対応 | 全モード (HTTP CONNECT) | Sora モードのみ (sora_sdk ProxyInfo)。P2P / Ayame は非対応 |
| TLS 証明書検証 | OpenSSL | rustls + OS ネイティブ証明書ストア (rustls_platform_verifier) |
| フェイクキャプチャ | WebRTC FakeVideoTrackSource | raden によるアニメーションフレーム生成 |
| ログ出力 | Boost.Beast + FileRotatingLogSink | tracing_subscriber (stdout のみ) |
| WebRTC 実装 | libwebrtc (C++) を直接使用 | shiguredo_webrtc (Rust バインディング) 経由で使用 |
| OpenH264 エンコーダー優先順位 | HW > OpenH264 > ビルトイン | `--use-v4l2-encoder` (HW) > `--openh264` (SW) > ビルトイン |

### 未実装機能

momo にあるが momo-rs に未実装の機能の一覧は [docs/MOMO.md](docs/MOMO.md) を参照してください。

## ライセンス

Apache License 2.0

```text
Copyright 2026 Shiguredo Inc.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```
