# momo と momo-rs の機能比較・TODO

momo-rs は momo の完全上位互換を目指す。

機能を実装した場合は本ファイルの momo-rs 列の状態を「実装済み」に更新すること。
新しい機能差分が見つかった場合は本ファイルに行を追加すること。

## 凡例

状態:

- 実装済み: 機能が動作する状態
- CLI のみ: CLI オプションは定義済みだが、パース後 `_` 付き変数に束縛されるだけで実処理に渡されていない
- 未実装: CLI オプション自体が存在しない

TODO:

- [ ] 未着手
- [x] 完了

## モード

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| P2P モード | 実装済み | 実装済み | HTTP サーバー + WebSocket シグナリング |
| Ayame モード | 実装済み | 実装済み | register/accept/reject/offer/answer/candidate |
| Sora モード | 実装済み | 実装済み | sora_sdk を使用。SoraServer (`--auto`) は未実装。`--port` は削除済み |

### TODO

- [x] Sora モード基本実装
  - [x] WebSocket シグナリング (sora_sdk 内部)
  - [x] DataChannel シグナリング (sora_sdk 内部)
  - [x] マルチストリーム (sora_sdk 内部)
  - [x] ロール制御 (sendonly / recvonly / sendrecv)
  - [x] メタデータ送信 (JSON)
  - [x] サイマルキャスト
  - [x] スポットライト (有効化のみ、配信数制御は sora_sdk API 不足)
  - [x] ビットレート制御 (映像 0-30000 / 音声 0-510)
  - [x] WebSocket 切断時の動作制御 (ignore-disconnect-websocket)
  - [x] メトリクス stats プロバイダー登録
- [x] Sora モード自動再接続
  - sora_sdk には再接続 API が無いため、momo-rs 側で接続ループを実装した
  - 接続が終了・失敗すると momo と同じ `5 * (2 * retry + 1)` 秒 (5, 15, 25, ...) 間隔で再接続する
- [ ] SoraServer (`--auto`) の実装
  - momo は HTTP API で実装済み: `GET /connect/status`, `GET /mute/status`, `POST /connect`, `POST /close`, `POST /mute`
- [x] Ayame モード自動再接続
  - momo と同じ watchdog 機構を実装 (初期 30 秒 → 接続後 60 秒、再接続間隔は 10 秒刻みで上限 30 秒、WebSocket 切断時は回数リセットで即再接続)
  - `ping` 受信でのみ監視期限を延ばす点は momo と同じ。ayame-labo の実測で `ping` は約 5 秒間隔のため誤再接続は起きない

### momo の P2P モード実装詳細

- HTTP 静的ファイルサーバー (`--document-root` でルートディレクトリ指定)
- WebSocket シグナリング (`/ws` エンドポイント)
- PeerConnection の作成・管理・破棄
- 映像・音声トラックの送受信
- ICE candidate の交換
- `StatsCollector` 実装でメトリクス API 対応

### momo-rs の P2P モード実装詳細

- HTTP 静的ファイルサーバー: shiguredo_http11 + tokio TcpListener
- WebSocket シグナリング: shiguredo_websocket_connection
- PeerConnection 管理: shiguredo_webrtc
- `--port` (デフォルト 8080)、`--document-root` (デフォルト `html`)
- メトリクス stats プロバイダー登録済み (`src/p2p/websocket.rs` で `peer.pc.get_stats()` を呼び出し)

### momo の Ayame モード実装詳細

- Ayame シグナリングサーバーへの WebSocket 接続
- register / accept / offer / answer / candidate / ping / bye メッセージ処理 (reject の処理はない。未知タイプは無視)
- direction: sendrecv / sendonly / recvonly
- コーデック指定: video-codec-type (VP8/VP9/AV1/H264/H265)、audio-codec-type (OPUS/PCMU/PCMA)
- `--client-id` 未指定時は 32 文字ランダムを生成、`--signaling-key` は register の `key` フィールドに設定
- ICE サーバー: サーバー返却の iceServers を優先。空で `--no-google-stun` 未指定時のみ Google STUN を追加
- watchdog 機構付き自動再接続: 初期 30 秒 → 接続は成功後 60 秒間隔に切り替え。リトライ間隔は 10 秒ステップで上限 30 秒。WebSocket 切断時は retry_count をリセットして即再接続
- `StatsCollector` 実装でメトリクス API 対応

### momo-rs の Ayame モード実装詳細

- Ayame シグナリングサーバーへの WebSocket 接続: shiguredo_websocket_connection (wss は rustls + rustls-platform-verifier)
- register / accept / reject / offer / answer / candidate / ping→pong / bye メッセージ処理 (momo と異なり reject に対応)
- direction: sendrecv / sendonly / recvonly
- コーデック指定: `--video-codec-type` (VP8/VP9/AV1/H264/H265)、`--audio-codec-type` (OPUS/PCMU/PCMA) バリデーション付き
- `--client-id` 未指定時はランダム 8 バイト hex を生成、`--signaling-key` は register の `key` フィールドに設定
- ICE サーバー: サーバー返却の iceServers を優先。無い場合は `--no-google-stun` 未指定時のみ Google STUN を追加
- メトリクス stats プロバイダー登録済み (`src/ayame/mod.rs` で `peer.pc.get_stats()` を呼び出し)
- 自動再接続: watchdog で死活を監視し、接続が終われば再接続する。WebSocket 接続を確立できていれば即再接続、確立できなければ `min(再接続回数 * 10, 30)` 秒待って再試行 (下限 1 秒)。`PeerConnectionFactory` とキャプチャは再接続をまたいで使い回し、WebSocket / PeerConnection / シグナリングタスクだけ作り直す。`bye` 受信時も再接続に入る
- シグナリングエラー (reject 含む) は接続失敗扱いにして間隔を伸ばす

### momo の Sora モード実装詳細

- WebSocket シグナリング (connect / offer / answer / re-offer / re-answer)
- DataChannel シグナリング (WebSocket からの切り替え、タイムアウト設定)。
  - `--data-channel-signaling-timeout` (デフォルト 180 秒): 切り替え後の watchdog タイムアウト
  - DataChannel の `signaling` / `stats` ラベル + zlib 圧縮対応
- マルチストリーム
- ロール制御 (sendonly / recvonly / sendrecv)
- メタデータ送信 (JSON)
- サイマルキャスト
- スポットライト (有効化 + `--spotlight-number` を connect メッセージへ転送のみ。クライアント側の配信数制御はない)
- 自動再接続: watchdog ベース。接続失敗時は `5 * (2*retry + 1)` 秒間隔で再試行。接続後は 60 秒間隔の ping-pong で監視
- ビットレート制御 (映像 0-30000 / 音声 0-510)
- WebSocket 切断時の動作制御 (ignore-disconnect-websocket)
- SoraServer (`--port` / `--auto`): 127.0.0.1 バインドの HTTP サーバー
  - `GET /connect/status` (ICE 状態), `GET /mute/status`
  - `POST /connect`, `POST /close`, `POST /mute` (body: `{"audio":bool,"video":bool}`)
  - `--auto` 指定時は接続開始、未指定の場合は接続 API を待つ。認証・CORS なし

### momo-rs の Sora モード実装詳細

- sora_sdk を使用した SoraClient の接続・実行
- 実装済み CLI オプション:
  - `--signaling-urls` (カンマ区切り複数 URL 対応), `--channel-id`, `--video`, `--audio`
  - `--video-codec-type` (VP8/VP9/AV1/H264/H265), `--audio-codec-type` (OPUS のみ。PCMU/PCMA はエラー), `--video-bit-rate` (0-30000), `--audio-bit-rate` (0-510)
  - `--role` (sendonly/recvonly/sendrecv), `--spotlight`, `--simulcast`
  - `--data-channel-signaling`, `--ignore-disconnect-websocket`, `--disconnect-wait-timeout`
  - `--metadata` (JSON), `--insecure` (+ `turn_tls_insecure`), `--cacert`
  - `--client-cert` / `--client-key` (同時指定必須), `--proxy-url` / `--proxy-username` / `--proxy-password` (sora_sdk の ProxyInfo)
  - `--client-id` / `--bundle-id` (1〜255 バイト検証付き), `--signaling-notify-metadata` (JSON), `--data-channel-label` (カンマ区切りの `#` 付きラベル), `--forwarding-filter` (JSON 配列)
  - `--openh264` / `--use-v4l2-encoder` / `--vp9-encoder` / `--av1-encoder` / `--h264-encoder` / `--h265-encoder` 等の VideoCodecCapability / VideoCodecPreference への反映
- 自動再接続: 接続が終了・失敗すると momo と同じ間隔式で再接続する。接続ごとに `SoraConnectionContext`・AudioDeviceModule・キャプチャ・トラックを作り直す (sora_sdk が `AdmConfig` 経由で AudioDeviceModule の所有を取るため、接続をまたいで AudioDeviceModule を共用できない)
- player 併用時: `VideoCapture` は Windows の Media Foundation バックエンドが COM 初期化を保持するため Send を実装しない。Sora 接続を `tokio::spawn` に乗せられず、専用スレッド上のカレントスレッドランタイムで実行する
- 接続イベントの可視化:
  - sora_sdk の `SoraConnectionEventHandler` を `src/sora/event_handler.rs` に実装
  - `on_notify` / `on_push` / `on_track` / `on_remove_track` / `on_switched` / `on_websocket_close` / `on_message` / `on_data_channel` 系を INFO 以上で出力、`on_signaling_message` は TRACE で出力
  - 受信トラックの表示 (player) はハンドラが持つレジストリで Sink の生存管理をする
- パース済みだが未使用:
  - `--auto` (SoraServer 機能が未実装)
  - `--data-channel-signaling-timeout` (sora_sdk に API なし)
- 対応しない:
  - `--port` (momo の SoraServer 用。momo-rs では SoraServer が未実装のため no-op としてだけ存在していたが、Sora 接続には不要なため削除済み)
  - `--spotlight-number` (momo-rs には CLI オプション自体が存在しない。momo にはあるが connect メッセージへ値を転送するのみで、クライアント側の意味論はない)
- 未対応:
  - `--degradation-preference` (sora_sdk API 不足。明示指定すると起動時にエラーになる)

## メトリクス API

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| `GET /metrics` エンドポイント | 実装済み | 実装済み | JSON レスポンス。それ以外は 404 / 非 GET は 400 |
| `--metrics-port` | 実装済み | 実装済み | デフォルト -1 (無効) |
| `--metrics-allow-external-ip` | 実装済み (localhost → 全 IP 切り替え) | 実装済み | 未指定時 `127.0.0.1`、指定時 `0.0.0.0` |
| レスポンス: `version` | 実装済み | 実装済み | momo: バージョン + コミットハッシュ、momo-rs: バージョンのみ |
| レスポンス: `environment` | 実装済み (OS 名/バージョン/アーキテクチャ詳細) | 実装済み | `[ARCH] OS_DETAIL` 形式 |
| レスポンス: `libwebrtc` | 実装済み (WEBRTC_READABLE_VERSION) | 実装済み | momo: `Shiguredo-Build {readable} ({build} {commit})`、momo-rs: `webrtc-rs {shiguredo_webrtc::version()}` (例: `webrtc-rs 0.152.1-canary.3`) |
| レスポンス: `stats` | 実装済み (RTCStatsReport) | 実装済み (RTCStatsReport) | 複数 PeerConnection 対応 (momo-rs) |
| CORS ヘッダー | なし | 実装済み | momo-rs のみ `Access-Control-Allow-Origin: *` を 200 レスポンスに付与 |

### TODO

- [x] `--metrics-allow-external-ip` の実装
- [x] レスポンスに `libwebrtc` フィールドを追加
- [x] レスポンスの `environment` を詳細化

### momo のメトリクス API 実装詳細

- HTTP サーバー: Boost.Beast (HTTP/1.1)
- `StatsCollector` インターフェースで PeerConnection から `RTCStatsReport` を取得
- P2P / Ayame / Sora 全モードが `StatsCollector` を実装
- レスポンス JSON:
  - `version`: `"WebRTC Native Client Momo {VERSION} ({COMMIT_SHORT})"`
  - `libwebrtc`: `"Shiguredo-Build {READABLE_VERSION} ({BUILD_VERSION} {COMMIT_SHORT})"`
  - `environment`: `"[{ARCH}] {OS_DETAIL}"` (Windows: OS バージョン+ビルド番号、macOS: OS 名+バージョン、Linux: /etc/os-release の PRETTY_NAME、Jetson: nvidia-l4t-core バージョン追加)
  - `stats`: `RTCStatsReport::ToJson()` の結果をそのまま JSON 配列として埋め込み
- バインドアドレス: `--metrics-allow-external-ip` なしの場合は `127.0.0.1`、ありの場合は `0.0.0.0`

### momo-rs のメトリクス API 実装詳細

- HTTP サーバー: shiguredo_http11 + tokio TcpListener (HTTP/1.1)
- `MetricsState` が複数の stats プロバイダー (`mpsc::Sender<oneshot::Sender<String>>`) を管理
- PeerConnection 生成時にプロバイダーを `MetricsState::register()` で登録
- P2P (`src/p2p/websocket.rs`)、Ayame (`src/ayame/mod.rs`)、Sora (`src/sora.rs`) で stats JSON を返す
- `collect_stats()` で全プロバイダーに oneshot channel で要求を送信し、各 PeerConnection の `RTCStatsReport` を JSON 配列として結合
- セッション終了済みのプロバイダーは送信失敗時に自動除去
- レスポンス JSON:
  - `version`: `"WebRTC Native Client Momo {CARGO_PKG_VERSION}"` 形式 (例: `"WebRTC Native Client Momo 2026.0.0"`。momo と異なりコミットハッシュは含まない)
  - `environment`: `"[ARCH] OS_DETAIL"` 形式 (例: `"[aarch64] macOS 15.3"`)
  - `libwebrtc`: `"webrtc-rs {shiguredo_webrtc::version()}"` 形式 (例: `"webrtc-rs 0.152.1-canary.3"`。momo の `Shiguredo-Build {readable} ({build} {commit})` 形式とは異なる)
  - `stats`: 各 PeerConnection の `report.to_json()` を結合した JSON 配列
- バインドアドレス: `--metrics-allow-external-ip` 未指定時 `127.0.0.1`、指定時 `0.0.0.0`
- CORS: 200 レスポンスのみ `Access-Control-Allow-Origin: *` を付与 (momo には CORS なし)
- エラーレスポンス: GET 以外は 400、`/metrics` 以外は 404

### momo のメトリクス API 実装詳細 (補足)

- StatsCollector は実行モードに対応する 1 個のみ (SoraClient / P2PServer / AyameClient の実装を 1 つ選択)
- CORS ヘッダーなし。認証なし。`--metrics-allow-external-ip` はバインドアドレスのみ変更

## 映像キャプチャ

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| V4L2 キャプチャ | 実装済み | 実装済み | |
| フェイクキャプチャ | 実装済み | 実装済み | `--fake-capture-device` |
| `--video-input-device` | 実装済み | 実装済み | momo: macOS/Windows はインデックスまたは名前、Linux は名前。momo-rs: DEVICE 名を渡す |
| `--no-video-input-device` | 実装済み | 実装済み | MomoConfig に含まれる |
| `--resolution` | 実装済み (QVGA/VGA/HD/FHD/4K/WxH) | 実装済み | QVGA/VGA/HD/FHD/4K/WxH 対応 |
| `--framerate` | 実装済み (1-120) | 実装済み (1-120) | 両実装とも 1-120 を検証する。範囲外は起動時に失敗 |
| `--force-i420` | 実装済み | 実装済み | VideoCaptureConfig.pixel_format に I420 を強制 |
| `--force-yuy2` | 実装済み | 実装済み | VideoCaptureConfig.pixel_format に YUY2 を強制 |
| `--force-nv12` | 実装済み | 実装済み | VideoCaptureConfig.pixel_format に NV12 を強制 |
| `--degradation-preference` | 実装済み (BALANCE/FRAMERATE/RESOLUTION) | 実装済み | momo: `--priority` (BALANCE/FRAMERATE/RESOLUTION) と `--fixed-resolution` で制御。momo-rs: balanced/maintain-framerate/maintain-resolution を DegradationPreference にマッピング。P2P/Ayame は RtpSender の RtpParameters に適用。maintain-framerate-and-resolution は webrtc-rs 待ち。Sora は sora_sdk API 不足のため明示指定時にエラー |
| スクリーンキャプチャ | 実装済み (WebRTC DesktopCapturer、ソースは sources[0] 固定) | CLI のみ | momo は `USE_SCREEN_CAPTURER` ビルド (Windows / macOS / Ubuntu x86_64) のみ。マルチディスプレイの選択 UI はなし。`ScreenVideoCapturer::GetSourceListString()` で一覧をログに出力のみ。momo-rs はパース後未使用。shiguredo_screen_capture crate の新規作成が必要 |

### TODO

- [x] `--resolution` の実装
- [x] `--framerate` の実装
- [x] `--force-i420` の実装
- [x] `--force-yuy2` の実装
- [x] `--force-nv12` の実装
- [x] `--degradation-preference` の実装 (P2P/Ayame/Sora は明示指定時にエラー)
- [ ] `--screen-capture` の実装 (pending)
  - 現状: CLI のみ。WebRTC DesktopCapturer、マルチディスプレイ対応
  - shiguredo_screen_capture crate の新規作成が必要 (macOS: ScreenCaptureKit / Linux Wayland: PipeWire / Linux X11: XShm / Windows: DXGI)

### momo の映像キャプチャ実装詳細

- V4L2VideoCapturer: 標準 V4L2 ビデオキャプチャ (Linux)
- フォーマット対応: I420, YUY2, NV12 + 各フォーマットの強制指定
- 解像度: QVGA (320x240), VGA (640x480), HD (1280x720), FHD (1920x1080), 4K (3840x2160), 任意 WxH
- フレームレート: 1-120fps で任意指定
- `--fixed-resolution`: WebRTC の映像品質劣化時に解像度を維持 (video track の ContentHint::kText で実装)
- `--priority`: BALANCE / FRAMERATE / RESOLUTION から選択 (RESOLUTION→MAINTAIN_RESOLUTION、FRAMERATE→MAINTAIN_FRAMERATE、BALANCE→BALANCED)
- スクリーンキャプチャ: WebRTC DesktopCapturer ベース、ソースは `sources[0]` 固定 (マルチディスプレイの選択 UI はなし)
- フェイクキャプチャ: WebRTC FakeVideoTrackSource (Blend2D 描画)

### momo-rs の映像キャプチャ実装詳細

- shiguredo_video_device による V4L2 キャプチャ (Linux)
- 対応ピクセルフォーマット: NV12, YUY2, I420 (自動検出、I420 に変換)
- 解像度: `--resolution` で QVGA/VGA/HD/FHD/4K/WxH 指定 (デフォルト VGA、WxH は正値のみ)
- フレームレート: `--framerate` で指定 (デフォルト 30fps。momo と同じく 1-120 を検証し、範囲外は起動時に失敗)
- `--degradation-preference`: balanced / maintain-framerate / maintain-resolution から選択し (デフォルト maintain-framerate 相当)、P2P / Ayame の映像送信パラメータに設定。maintain-framerate-and-resolution は webrtc-rs 待ち。Sora では sora_sdk API 不足のため明示指定時にエラー
- フェイクキャプチャ: raden でアニメーションフレーム生成、指定解像度・フレームレート対応
- `--force-i420` / `--force-yuy2` / `--force-nv12`: `VideoCaptureConfig.pixel_format` に強制指定 (排他、同時指定不可)

## 音声

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| `--audio-input-device` | 実装済み | 実装済み | MomoConfig に含まれる |
| `--audio-output-device` | 実装済み | 実装済み | インデックス番号・デバイス名・unique_id で指定。失敗時はデフォルトにフォールバック + 警告 |
| `--no-audio-device` | 実装済み | 実装済み | MomoConfig に含まれる |
| `--disable-echo-cancellation` | 実装済み (WebRTC BuiltinAudioProcessing) | CLI のみ | パース後未使用 |
| `--disable-auto-gain-control` | 実装済み (WebRTC BuiltinAudioProcessing) | CLI のみ | パース後未使用 |
| `--disable-noise-suppression` | 実装済み (WebRTC BuiltinAudioProcessing) | CLI のみ | パース後未使用 |
| `--disable-highpass-filter` | 実装済み (WebRTC BuiltinAudioProcessing) | CLI のみ | パース後未使用 |

### TODO

- [ ] 音声処理オプション全般 (pending)
  - shiguredo_webrtc API 不足のため pending
  - 対象: `--disable-echo-cancellation`, `--disable-auto-gain-control`, `--disable-noise-suppression`, `--disable-highpass-filter`

### momo の音声処理実装詳細

- WebRTC BuiltinAudioProcessing で音声処理パイプラインを構成
- エコーキャンセレーション (AEC): デフォルト有効、`--disable-echo-cancellation` で無効化
- 自動ゲイン制御 (AGC): デフォルト有効、`--disable-auto-gain-control` で無効化
- ノイズ抑制 (NS): デフォルト有効、`--disable-noise-suppression` で無効化
- ハイパスフィルター: デフォルト有効、`--disable-highpass-filter` で無効化
- 入力デバイス指定: インデックス番号またはデバイス名 (GUID 完全一致)
- 出力デバイス指定: インデックス番号またはデバイス名 (GUID 完全一致、失敗時はデフォルトにフォールバック + 警告)

### momo-rs の音声実装詳細

- shiguredo_audio_device による音声デバイス列挙・選択
- `--audio-input-device`: インデックス番号・デバイス名・unique_id で指定 (MomoConfig に含まれる)
- `--no-audio-device`: 音声を無効化 (MomoConfig に含まれる)
- `--audio-output-device`: インデックス番号・デバイス名・unique_id で指定 (MomoConfig に含まれる)。ADM 実装 (`src/adm.rs`) は録音 (mic) 方向 (`on_audio_frame` → `recorded_data_is_available`) と再生 (playout) 方向 (`start_playout` → `need_more_play_data` → AudioPlayback) の両方を実装
- 音声処理オプション (AEC/AGC/NS/ハイパスフィルター) は CLI でパースされるが実処理に渡されていない (AudioProcessingBuilder::new_builtin() 固定)

## コーデック

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| VP8 (Software) | 実装済み | 実装済み | |
| VP9 (Software) | 実装済み | 実装済み | |
| AV1 (Software) | 実装済み | 実装済み | |
| H.264 (Software) | 実装済み | 実装済み | |
| H.265 | 実装済み | 実装済み | |
| `--vp8-encoder` / `--vp8-decoder` | 実装済み (default/software/jetson/nvidia/vpl/videotoolbox/v4l2) | 実装済み (Sora / P2P / Ayame) | P2P/Ayame は sora_sdk の VideoCodecCapability 経由で software / nvidia / vpl / amf / videotoolbox を選択。jetson は未実装 |
| `--vp9-encoder` / `--vp9-decoder` | 実装済み | 実装済み (Sora / P2P / Ayame) | 同上 |
| `--av1-encoder` / `--av1-decoder` | 実装済み | 実装済み (Sora / P2P / Ayame) | 同上 (nvidia / vpl / amf) |
| `--h264-encoder` / `--h264-decoder` | 実装済み | 実装済み (Sora / P2P / Ayame) | P2P/Ayame は videotoolbox / nvidia / vpl / amf 指定時のみ capability 経由。`v4l2` は `--use-v4l2-encoder` と同じ扱い |
| `--h265-encoder` / `--h265-decoder` | 実装済み | 実装済み (Sora / P2P / Ayame) | 同上 (videotoolbox は macOS/iOS) |
| `--video-codec-engines` | 実装済み (利用可能コーデック一覧表示) | 実装済み | sora_sdk の VideoCodecCapability を使用して一覧表示 |
| `--openh264` | 実装済み | 実装済み | OpenH264 ソフトウェア H.264 エンコーダー/デコーダー。動的ライブラリパスを指定して利用 |
| `--hw-mjpeg-decoder` | 実装済み | CLI のみ | momo は Jetson (NvJPEG) / NVIDIA (CUDA JPEG) / V4L2 (ハードウェアコーデック) のビルド時に利用可能。momo-rs はパース後未使用 |
| H.264 HW エンコード (V4L2) | 実装済み | 実装済み | `--use-v4l2-encoder` (raspberrypi feature) |
| H.264 HW エンコード (Jetson) | 実装済み (USE_JETSON_ENCODER) | 未実装 | |
| H.264/H.265 HW エンコード (NVIDIA) | 実装済み (USE_NVCODEC_ENCODER/CUDA) | 実装済み (Sora / P2P / Ayame) | momo-rs は `nvcodec` feature + `--h264-encoder nvidia` 等の明示指定 |
| H.264/H.265 HW エンコード (Intel) | 実装済み (USE_VPL_ENCODER/oneVPL) | 実装済み (Sora / P2P / Ayame) | momo-rs は `vpl` feature + `--vp9-encoder vpl` 等の明示指定 (VP9 も対応) |
| H.264/H.265 HW エンコード (AMD) | なし | 実装済み (Sora / P2P / Ayame) | momo-rs 独自。`amf` feature + `--h264-encoder amf` 等の明示指定 |
| H.264/H.265 HW エンコード (VideoToolbox) | 実装済み (macOS) | 実装済み | `--h264-encoder videotoolbox` / `--h265-encoder videotoolbox` で明示指定。sora_sdk の VideoCodecPreference で設定 |

### TODO

- [x] `--h264-encoder` / `--h264-decoder` の実装 (videotoolbox 指定)
- [x] `--h265-encoder` / `--h265-decoder` の実装 (videotoolbox 指定)
- [x] `--video-codec-engines` の実装
  - sora_sdk の InternalVideoCodecCapability / InternalHwaVideoCodecCapability を使用
  - 各コーデック (VP8/VP9/AV1/H264/H265) のエンコーダー/デコーダー対応状況を表示
- [x] `--vp8-encoder/decoder` の実装
  - Sora は常に sora_sdk ビルトイン VP8 を使う
  - P2P/Ayame は sora_sdk の capability 経由で software / nvidia / vpl / amf / videotoolbox を選択できる
- [x] `--vp9-encoder/decoder`, `--av1-encoder/decoder` の実装 (Sora / P2P / Ayame)
  - Sora は VideoCodecPreference に設定 (nvidia / vpl / amf)
  - P2P/Ayame は capability ベースのファクトリで選択する
- [ ] Jetson バックエンド (`--{codec}-encoder jetson`) の実装
- [ ] `--hw-mjpeg-decoder` の実装 (pending)
  - 現状: パース後未使用
- [x] `--openh264` の実装 (ソフトウェア H.264 エンコーダー/デコーダー)
  - shiguredo_openh264 crate による Cisco OpenH264 ソフトウェアコーデックの統合
  - HW エンコーダーが利用できない環境での H.264 対応手段
  - P2P / Ayame / Sora 全モードで利用可能
- [ ] H.264 HW エンコード (Jetson) の実装
  - 現状: 未実装。momo は USE_JETSON_ENCODER で H.264/H.265 対応
- [x] H.264/H.265 HW エンコード (NVIDIA) の実装 (Sora / P2P / Ayame)
  - `nvcodec` feature で sora_sdk の NvCodecVideoCodecCapability を登録し、`--h264-encoder nvidia` 等で選択
- [x] H.264/H.265 HW エンコード (Intel) の実装 (Sora / P2P / Ayame)
  - `vpl` feature で sora_sdk の VplVideoCodecCapability を登録し、`--vp9-encoder vpl` 等で選択 (VP9 も対応)
- [x] H.264/H.265/AV1 HW エンコード (AMD AMF) の実装 (Sora / P2P / Ayame)
  - momo にはない momo-rs 独自機能。`amf` feature で sora_sdk の AmfVideoCodecCapability を登録し、`--h264-encoder amf` 等で選択
- [x] H.264/H.265 HW エンコード (VideoToolbox)
  - shiguredo_webrtc が標準で VideoToolbox を使用するため追加実装不要

### momo のエンコーダー/デコーダー実装詳細

- **OpenH264** (`--openh264`): Cisco OpenH264 動的ライブラリによるソフトウェア H.264 エンコード/デコード
  - VideoCodecInfo::Type には OpenH264 番号はなく、Software の内部実装。`--openh264` 指定時は h264_encoder を Software に強制
- **Jetson** (USE_JETSON_ENCODER): NVIDIA Jetson ハードウェアエンコーダー (H.264/H.265)
- **NVIDIA** (USE_NVCODEC_ENCODER): CUDA 対応 GPU の NvCodec エンコーダー/デコーダー (H.264/H.265)
- **Intel** (USE_VPL_ENCODER): oneVPL (旧 Intel Media SDK) エンコーダー (H.264/H.265)
- **VideoToolbox** (macOS): Apple VideoToolbox フレームワーク (H.264/H.265)
- **V4L2** (USE_V4L2_ENCODER): Raspberry Pi V4L2 M2M ハードウェアエンコーダー (H.264)
- ソフトウェアデコーダーは VP8/VP9/AV1 のみ。ソフトウェア H.264/H.265 のデコードは不可 (H264 SW デコーダーは nullptr)。ソフトウェア H.265 エンコーダーもなし
- エンコーダー選択: `--{codec}-encoder` で `default` / `software` / HW バックエンド名を指定
- `--video-codec-engines`: 利用可能な全エンコーダー/デコーダーの組み合わせを一覧表示 (`*UNAVAILABLE*` を含む)

### momo-rs のエンコーダー/デコーダー実装詳細

#### OpenH264 ソフトウェアエンコーダー/デコーダー (`--openh264`)

- Cisco OpenH264 によるソフトウェア H.264 エンコード/デコード
- HW エンコーダーが利用できない環境 (x86_64 Linux、macOS 等) での H.264 対応手段
- shiguredo_openh264 crate で OpenH264 動的ライブラリを実行時に読み込む
- `--openh264 <PATH>` で共有ライブラリパスを指定 (macOS: `.dylib`, Linux: `.so`, Windows: `.dll`)
- **エンコーダー**: `VideoEncoderFactoryHandler` を実装し、H.264 は OpenH264 で処理
- **デコーダー**: `VideoDecoderFactoryHandler` を実装し、H.264 は OpenH264 で処理
- **Sora**: `VideoCodecCapability` を実装し、エンコード/デコード両方に対応
- H.264 Constrained Baseline Profile (42e01f) を優先
- H.264 以外のコーデック要求時はビルトインエンコーダー/デコーダーにフォールバック
- エンコーダー: init_encode / encode / release / set_rates コールバック実装
- デコーダー: configure / decode / release コールバック実装
- キーフレーム要求 (force IDR) 対応、ビットレート動的変更対応
- I420 (YUV 4:2:0 planar) 入出力、ストライド変換対応
- 優先順位: `--use-v4l2-encoder` (HW) > `--openh264` (SW) > ビルトイン

#### V4L2 (`--use-v4l2-encoder`)

- shiguredo_v4l2 の `H264Encoder` を使用した V4L2 M2M ハードウェアエンコーディング (raspberrypi feature)
- `VideoEncoderFactory` のカスタム実装で WebRTC に統合
- H.264 Constrained Baseline Profile (42e0) を優先
- H.264 以外のコーデック要求時はビルトインソフトウェアエンコーダーにフォールバック
- init_encode / encode / release / set_rates コールバック実装
- キーフレーム要求対応、ビットレート動的変更対応

#### P2P / Ayame のコーデック選択 (`--{codec}-encoder` / `--{codec}-decoder`)

- `src/video_codec_factory.rs` に Sora と同じ sora_sdk の `VideoCodecCapability` を組み合わせた
  `VideoEncoderFactory` / `VideoDecoderFactory` を実装
- 指定可能なバックエンド: `default` / `software` (internal)、`nvidia` (nvcodec)、`vpl` (Intel)、
  `amf` (AMD)、`videotoolbox` (macOS / iOS)
- 指定しないコーデックはビルトイン (software) を使う
- `--h264-encoder v4l2` は `--use-v4l2-encoder` と同じ扱いにする
- `--openh264` / `--use-v4l2-encoder` を指定した場合は従来の専用ファクトリを優先し、
  `--{codec}-encoder` / `--{codec}-decoder` の指定は無視する
- jetson バックエンドは未実装

## libcamera (Raspberry Pi)

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| `--use-libcamera` | 実装済み | 実装済み | raspberrypi feature で有効化 |
| `--use-libcamera-native` | 実装済み (DMA-BUF ネイティブバッファをエンコーダに直接渡す) | 実装済み | DMA-BUF fd を V4L2 M2M エンコーダーにゼロコピーで渡す。simulcast 時は無効 |
| `--libcamera-control KEY=VALUE` | 実装済み (キー/バリュー形式、momo は `KEY VALUE` の2引数形式) | 実装済み | momo-rs は `KEY=VALUE` 形式。複数回指定可能 |

### TODO

- [x] `--use-libcamera-native` の実装
- [x] `--libcamera-control KEY=VALUE` の実装

### momo の libcamera 実装詳細

- sora-cpp-sdk の `LibcameraCapturer` を使用
- `--use-libcamera-native`: DMA-BUF ネイティブバッファをエンコーダに直接渡す (ゼロコピー)
  - I420Buffer 変換をスキップしてメモリコピーを削減
- `--libcamera-control KEY VALUE`: libcamera のコントロールパラメータを任意に設定
  - 露出、ホワイトバランス、明るさ等のカメラ制御

### momo-rs の libcamera 実装詳細

- shiguredo_libcamera を使用した独自実装 (`src/libcamera.rs`)
- CameraManager → カメラ取得 → StreamRole::VideoRecording でストリーム設定
- YU12 (I420) ピクセルフォーマット固定
- FrameBufferAllocator によるフレームバッファ管理
- DMA-BUF を mmap して読み取り (通常モード)
- `--use-libcamera-native`: DMA-BUF fd を V4L2 M2M エンコーダーにゼロコピーで渡す
  - タイムスタンプをキーにした共有マップで libcamera スレッド → エンコードスレッド間の fd 受け渡し
  - `--use-libcamera` + `--use-v4l2-encoder` が前提、simulcast 時は自動無効化
  - エンコード完了を待機してからバッファを requeue (DMA-BUF の安全な再利用)
- TimestampAligner で WebRTC タイムスタンプに変換
- AdaptedVideoTrackSource の `adapt_frame()` でフレームスケーリング対応
- キャプチャは専用スレッドで実行、`AtomicBool` で停止制御
- Drop 時にキャプチャスレッドを自動停止
- `--libcamera-control KEY=VALUE`: コントロールパラメータの設定 (複数回指定可能)
  - 対応型: Bool, Int32, Int64, Float, Rectangle, 各配列型
  - 主要 enum の文字列指定に対応 (AfMode, AwbMode, AeMeteringMode 等)
  - momo は `KEY VALUE` (2引数) 形式、momo-rs は `KEY=VALUE` (1引数) 形式
  - Request ごとにコントロールを適用 (reuse 後も再適用)
  - 未知のコントロール名やパース失敗時は警告を出力して他のコントロールは適用継続

## ネットワーク・セキュリティ

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| `--no-google-stun` | 実装済み | 実装済み | P2P / Ayame モードで Google STUN サーバー追加をスキップ。Sora モードは sora_sdk 内部で ICE 管理のため対象外 |
| `--insecure` | 実装済み | 実装済み | Ayame: rustls NoVerifier、Sora: sora_sdk insecure + turn_tls_insecure |
| `--cacert` | **なし** | 実装済み | CA 証明書指定 (PEM)。momo にはない momo-rs 独自機能 |
| `--proxy-url` | 実装済み (CONNECT トンネリング + Basic Auth) | 実装済み (Sora のみ) | sora_sdk の ProxyInfo API に渡す。P2P / Ayame は非対応。momo は ICE (STUN) 接続もプロキシ経由 (PROXY_HTTPS) |
| `--proxy-username` | 実装済み | 実装済み (Sora のみ) | P2P / Ayame は非対応 |
| `--proxy-password` | 実装済み | 実装済み (Sora のみ) | P2P / Ayame は非対応 |
| `--client-cert` | 実装済み (PEM, TLS 1.2/1.3) | 実装済み | Ayame: rustls + rustls-pemfile、Sora: sora_sdk |
| `--client-key` | 実装済み (PEM) | 実装済み | --client-cert と同時指定必須 |

### TODO

- [x] `--insecure` の実装
- [x] `--cacert` の実装
- [x] `--proxy-url` / `--proxy-username` / `--proxy-password` の実装
  - Sora モードのみ対応。sora_sdk の `ProxyInfo` API に渡す
  - P2P / Ayame モードは非対応
- [x] `--client-cert` の実装
- [x] `--client-key` の実装

### momo のネットワーク・セキュリティ実装詳細

- **WSS サーバー証明書検証**: X509 検証 (OS の証明書ストア + ISRG Root X1 内蔵 + WebRTC 組み込みルート) に加えて**ホスト名検証**を実施 (`X509_check_host` / `X509_check_ip`)。2026.1 canary で追加 (momo 側 #463 / #462)。IPv6 ブラケット・末尾ドットは正規化
- **プロキシ**: HTTP CONNECT メソッドでトンネリング、Basic 認証対応、WebSocket + SSL 通信に対応 (http プロキシ + wss ターゲットのみ)
- **ICE プロキシ**: WebRTC 側の ICE (STUN) 接続もプロキシ経由 (BasicPortAllocator::set_proxy)
- **クライアント証明書**: PEM 形式の証明書・秘密鍵ファイル読み込み、TLS 1.2/1.3 対応、カスタム SSL 検証 (`ssl_verifier.h`)
- **insecure**: SSL 証明書 (ホスト名を含む) の検証をスキップ (開発用途。DTLS 側はホスト名検証なし、証明書チェーン検証のみ)

### momo-rs のネットワーク・セキュリティ実装詳細

#### TLS 証明書検証

WSS (WebSocket over TLS) の証明書検証には **`rustls_platform_verifier`** を使用している。
OS のネイティブ証明書ストアを利用する (macOS: Security.framework、Linux: OpenSSL 証明書ストア)。
webpki-roots (内蔵証明書バンドル) は使っていない。

- **Ayame モード**: `rustls::ClientConfig::with_platform_verifier()` で TLS 接続を構築
- **Sora モード**: sora_sdk 内部で同じく `rustls::ClientConfig::with_platform_verifier()` を使用

#### `--insecure` の実装

- **Ayame モード**: rustls の `dangerous()` API で `NoVerifier` (全証明書を受け入れるダミー検証器) を設定
- **Sora モード**: sora_sdk の `.insecure(true)` で WSS 証明書検証スキップ + `.turn_tls_insecure(true)` で TURN-TLS 証明書検証スキップ

#### TURN-TLS の証明書検証 (Sora モード)

sora_sdk 内部では TURN-TLS の証明書検証に **rustls-webpki** (`rustls-webpki` 0.103) を使用している。
shiguredo_webrtc の `SSLCertificateVerifier` コールバック経由で `TurnTlsCaCertVerifier` を実装し、
`webpki::EndEntityCert::verify_for_usage()` で証明書チェーンを検証する。

- `turn_tls_ca_cert` 指定時: DER 形式の CA 証明書を `webpki::anchor_from_trusted_cert()` で `TrustAnchor` としてロードし、`PeerConnectionDependencies::set_tls_cert_verifier()` に設定する
- `turn_tls_insecure` 指定時: WebRTC の ICE サーバー設定で `TlsCertPolicy::InsecureNoCheck` を設定する

#### `--client-cert` / `--client-key` の実装

- PEM ファイルを読み込み、`(cert_pem, key_pem)` タプルとして各モードに伝搬
- `--client-cert` と `--client-key` は同時指定必須 (片方のみはエラー)
- **Ayame モード**: `rustls_pki_types` で PEM をパースし、`with_client_auth_cert()` または `SingleCertResolver` で rustls に設定
- **Sora モード**: `sora_sdk::SoraClientBuilder::client_cert(cert_pem, key_pem)` で設定
- `rustls-pki-types` v1 を依存に追加

#### `--cacert` の実装

- momo にはないオプションのため momo-rs 独自機能
- PEM 形式の CA 証明書ファイルパスを指定
- **Ayame モード**: `RootCertStore` に PEM をロードし `with_root_certificates()` で設定 (platform verifier を使わない)
- **Sora モード**: `sora_sdk::SoraClientBuilder::ca_cert(ca_pem)` で設定
- `--insecure` と同時指定時は `--insecure` が優先 (`--cacert` は無視)

## データチャネル

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| シリアルデータチャネル | 実装済み (Boost ASIO 非同期シリアル通信、DataChannel パススルー) | 実装済み | P2P / Ayame モードで利用可能。Linux 限定 |

### TODO

- [x] シリアルデータチャネルの実装
  - `--serial DEVICE,BAUDRATE` でシリアルポートを開き、"serial" ラベルの DataChannel と双方向ブリッジ
  - std::fs + libc termios + tokio AsyncFd で実装 (外部依存なし)
  - P2P / Ayame モードで利用可能。Linux 限定

### momo のシリアルデータチャネル実装詳細

- `SerialDataManager`: シリアルポートの管理と DataChannel の紐付け
- `SerialDataChannel`: WebRTC DataChannel を通じたシリアルデータのパススルー
- Boost ASIO ベースの非同期シリアル通信
- DataChannel でバイナリデータを受信 → シリアルポートに書き込み
- シリアルポートからデータ受信 → DataChannel で送信
- `--serial DEVICE,BAUDRATE` 形式 (例: `--serial /dev/ttyUSB0,9600`)
- P2P / Ayame モードで利用可能

### momo-rs のシリアルデータチャネル実装詳細

- `src/serial.rs` に実装。Linux 限定 (`#[cfg(target_os = "linux")]`)
- シリアルポートの開設: `std::fs::OpenOptions` + `libc::tcsetattr` / `cfmakeraw` / `cfsetspeed` で raw モード設定
- 非同期化: `libc::fcntl` で `O_NONBLOCK` 設定後、`tokio::io::unix::AsyncFd` でラップ
- DataChannel 受信: `DataChannelObserverHandler::on_message` → `mpsc::UnboundedSender` 経由で tokio タスクに転送 → シリアルポートに書き込み
- シリアルポート受信: `AsyncFd::readable()` で非同期待ち → `DataChannel::send()` でバイナリ送信
- ブラウザ側が "serial" ラベルの DataChannel を作成し、momo-rs は `on_data_channel` コールバックで受け取る
- 対応ボーレート: 1200 / 2400 / 4800 / 9600 / 19200 / 38400 / 57600 / 115200 / 230400 / 460800 / 500000 / 576000 / 921600 / 1000000 / 1500000 / 2000000
- 外部依存なし (tokio-serial 等を使わず libc + tokio AsyncFd で実装)

## 表示

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| SDL レンダラー | 実装済み (`--use-sdl`, SDL3, 受信映像表示) | 実装済み (`--player`, 受信映像のグリッド表示) | momo も momo-rs も受信した複数ストリームを 1 ウィンドウ内にグリッド表示する。ローカルプレビューは表示しない |
| `--player` | なし (momo は `--use-sdl`) | 実装済み | player feature (デフォルト有効)。Sora の受信映像 (recvonly / sendrecv) を SDL3 グリッド表示 |
| `--fullscreen` | 実装済み | CLI のみ | momo は起動時に設定 + 実行中に F キーでトグル。momo-rs はパース後未使用 |
| `--window-width` / `--window-height` | 実装済み | 実装済み | 表示ウィンドウサイズ指定 (デフォルト 640x480)。momo は範囲検証あり (180-16384) |

### TODO

- [ ] `--fullscreen` の実装
  - 現状: CLI のみ。パース後未使用

### momo の表示実装詳細

- SDL レンダラー: SDL3 を使用した受信映像の表示ウィンドウ
- `--window-width` / `--window-height`: ウィンドウサイズ指定 (デフォルト 640x480)
- `--fullscreen`: フルスクリーン表示
- `--use-raw-player`: Raw プレイヤーでの映像表示 (SDL の代替)

### momo-rs の表示実装詳細

- raw_player crate (SDL3 ベース) を使用。player feature (デフォルト有効)
- `--player`: Sora の受信映像を SDL3 ウィンドウにグリッド表示
  - メインスレッドで SDL3 イベントループを実行、Sora 接続は別タスクで起動
  - sora_sdk の `on_track` で受信した VideoTrack に VideoSink をアタッチし、I420 フレームを受信
  - ストリーム数とウィンドウアスペクト比に応じてグリッド (列数) を自動計算し、アスペクト比を保ってセル内に配置
  - 最新フレーム優先 (容量 1 の bounded チャネル) でベストエフォート転送。送信プレビュー (ローカルのカメラ映像) は表示しない
  - `--window-width` / `--window-height` でウィンドウサイズ指定 (デフォルト 640x480)
- VideoSink の生存期間は Sora 接続中に限定しており、表示ウィンドウを閉じた後も安全に接続を継続できる

## ログ・デバッグ

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| `--log-level` | 実装済み (verbose/info/warning/error/none) | 実装済み | tracing_subscriber EnvFilter で適用 |
| ログファイル出力 | 実装済み (FileRotatingLogSink, 10MB x 10 ファイル、`webrtc_logs_*`) | 未実装 | stdout のみ |
| `--list-devices` | 実装済み | 実装済み | momo: テキスト形式、momo-rs: JSON 形式 |

### TODO

- [x] `--log-level` の実装
- [ ] ログファイル出力の実装
  - 現状: 未実装。stdout のみ
  - momo: FileRotatingLogSink, `webrtc_logs_*`, 10MB x 10 ファイル

### momo のログ実装詳細

- `--log-level`: verbose / info / warning / error / none の 5 段階
- ファイル出力: `webrtc::FileRotatingLogSink` でカレントディレクトリに出力
  - ファイル名プレフィックス: `webrtc_logs`
  - 最大ファイルサイズ: 10MB
  - ローテーション: 最大 10 ファイル

### momo-rs のログ実装詳細

- `init_tracing()` でサブコマンド処理前に `tracing_subscriber` を初期化
- `--log-level` を `tracing_subscriber::EnvFilter` に変換 (verbose=TRACE, info=INFO, warning=WARN, error=ERROR, none=OFF)
- ファイル出力の仕組みは未実装

### momo-rs のデバイス一覧実装詳細

- `--list-devices`: shiguredo_video_device と shiguredo_audio_device で列挙
- JSON 形式で出力 (nojson クレート使用。momo はテキスト形式)
- ビデオデバイス: name, unique_id, formats (width, height, min_fps, max_fps, pixel_format)
- オーディオ入力デバイス: name, unique_id, channels, sample_rate (オーディオ出力デバイスは列挙しない。momo は入力・出力とも列挙し、テキスト形式で表示)

## バージョン情報

| 機能 | momo | momo-rs | 備考 |
|------|------|---------|------|
| バージョン番号 | 実装済み (MOMO_VERSION) | 実装済み (CARGO_PKG_VERSION) | |
| コミットハッシュ | 実装済み (MOMO_COMMIT_SHORT) | 実装済み | build.rs で `git rev-parse --short HEAD` |
| libwebrtc バージョン | 実装済み (WEBRTC_READABLE_VERSION + BUILD_VERSION) | 実装済み | `Shiguredo-Build {shiguredo_webrtc::version()}` |
| 環境情報 | 実装済み (OS 名/バージョン/アーキテクチャ、Jetson L4T バージョン) | 実装済み | `[ARCH] OS_DETAIL` 形式 |
| ビルドフラグ | 実装済み (USE_JETSON_ENCODER, USE_NVCODEC_ENCODER, USE_V4L2_ENCODER, USE_VPL_ENCODER) | 実装済み | ayame / sora は常時、raspberrypi / player / nvcodec / vpl / amf は feature に応じて表示 |

### TODO

- [x] `--version` にコミットハッシュを追加
- [x] `--version` に libwebrtc バージョンを追加
- [x] `--version` に環境情報を追加
- [x] `--version` にビルドフラグを追加

### momo の `--version` 出力例

```
WebRTC Native Client Momo 2024.1.0 (abc1234)
WebRTC: Shiguredo-Build M120 (6099.0 def5678)
Environment: [aarch64] Ubuntu 22.04.3 LTS (nvidia-l4t-core 35.4.1-20230801210015)
```

- 1 行目: momo バージョン + コミットハッシュ
- 2 行目: libwebrtc のビルドバージョン (Shiguredo-Build + Readable Version + Build Version + Commit Short)
- 3 行目: 環境情報
  - Windows: `[x64] Windows 10.0 Build 19045`
  - macOS: `[arm64] macOS 14.2`
  - Linux: `[aarch64] Ubuntu 22.04.3 LTS`
  - Jetson: 上記に加えて `(nvidia-l4t-core バージョン)` を追加
- コンパイルオプション表示: USE_JETSON_ENCODER, USE_NVCODEC_ENCODER, USE_V4L2_ENCODER, USE_VPL_ENCODER

### momo-rs の `--version` 出力

```
shiguredo_momo 2026.0.0 (abc1234)
WebRTC: Shiguredo-Build 0.152.1-canary.3
OpenH264: v2.6.0 (build)
Environment: [aarch64] macOS 15.3
Build Flags: ayame, sora
```

- 1 行目: パッケージ名 + バージョン + コミットハッシュ (build.rs で埋め込み)
- 2 行目: shiguredo_webrtc のクレートバージョン
- 3 行目: shiguredo_openh264 のビルド時 OpenH264 バージョン
- 4 行目: `[ARCH] OS_DETAIL` 形式の環境情報
- 5 行目: 有効なビルドフラグ。ayame / sora は常時表示。player / raspberrypi は feature に応じて追加される

## E2E テスト

| テストファイル | テスト数 | カバー機能 |
|---------------|---------|-----------|
| `test_p2p_mode.py` | 4 | P2P モード起動、カスタム引数、マルチインスタンス並行動作、動的生成・削除 |
| `test_ayame_mode.py` | 9 | Ayame モード起動、client_id、ビデオ/オーディオ設定、無効コーデックエラー |
| `test_momo_validation.py` | 9 | モード間オプション混在エラー、共通オプション |
| `test_metrics_api.py` | 10 | メトリクス API: JSON 構造、version/libwebrtc/environment フォーマット、stats 構造、404/400、`--metrics-allow-external-ip` |
| `test_sora_mode.py` | 3 | Sora モードのメトリクスエンドポイント 200 / 構造 / 404 |
| `test_sora_mode_apple_video_toolbox.py` | 3 | Apple VideoToolbox H264/H265 エンコーダー/デコーダー、sendonly/recvonly ペア |
| `test_sora_mode_data_channel_signaling.py` | 1 | DataChannel シグナリング (`--data-channel-signaling`) |
| `test_sora_mode_ignore_disconnect_websocket.py` | 1 | `--ignore-disconnect-websocket` (DataChannel シグナリング併用) |
| `test_sora_mode_sendonly_recvonly.py` | 2 | sendonly/recvonly ペアの RTP 送受信 |
| `test_sora_mode_sendrecv.py` | 1 | sendrecv 接続と RTP 送受信 |
| `test_sora_mode_simulcast.py` | 1 | simulcast 接続 |
| `test_sora_mode_openh264.py` | 3 | OpenH264 H264 ソフトウェアエンコーダー/デコーダー |
| `test_sora_mode_nv12.py` | 1 | NV12 キャプチャフォーマット |
| `test_sora_mode_nvidia_video_codec.py` | 3 | NVIDIA NvCodec H264/H265/AV1 エンコーダー/デコーダー、simulcast |
| `test_sora_mode_intel_vpl.py` | 3 | Intel oneVPL VP9/AV1/H264/H265 エンコーダー/デコーダー、simulcast |
| `test_sora_mode_raspberry_pi.py` | 3 | Raspberry Pi: libcamera キャプチャ + V4L2 M2M エンコーダー (self-hosted runner) |

### TODO

実装済み:
- [x] P2P モード起動・メトリクス確認・複数インスタンス (test_p2p_mode.py)
- [x] Ayame モード起動・設定バリエーション・不正コーデック検証 (test_ayame_mode.py)
- [x] モード固有オプションの検証 (test_momo_validation.py)
- [x] momo.py: Momo クラス、get_metrics()、wait_for_connection()、wait_stats 対応
- [x] Sora モード E2E テスト
  - 基本 (test_sora_mode.py)、sendonly/recvonly ペア (test_sora_mode_sendonly_recvonly.py)、sendrecv (test_sora_mode_sendrecv.py) で RTP 送受信を検証
  - simulcast (test_sora_mode_simulcast.py)、DataChannel シグナリング (test_sora_mode_data_channel_signaling.py)、`--ignore-disconnect-websocket` (test_sora_mode_ignore_disconnect_websocket.py)
  - `--ignore-disconnect-websocket` は接続切断検知の要修正を含む (sora_sdk 側のバグで xfail を付与。issue 0038)
- [x] Apple Video Toolbox E2E テスト (test_sora_mode_apple_video_toolbox.py)
  - H264/H265 VideoToolbox エンコーダーで sendonly 接続、encoderImplementation 確認
  - sendonly/recvonly ペアで encoderImplementation/decoderImplementation が VideoToolbox であることを確認
  - GitHub Actions の self-hosted runner (macOS ARM64) で CI 実行
- [x] NVIDIA NvCodec E2E テスト (test_sora_mode_nvidia_video_codec.py)
  - H264/H265/AV1 の接続確立・simulcast・sendonly/recvonly ペアを検証 (self-hosted runner)
- [x] Intel oneVPL E2E テスト (test_sora_mode_intel_vpl.py)
  - VP9/AV1/H264/H265 の接続確立・simulcast を検証 (self-hosted runner)
- [x] OpenH264 E2E テスト (test_sora_mode_openh264.py)
  - H264 ソフトウェアエンコーダー/デコーダーの接続確立を検証
- [x] NV12 キャプチャ E2E テスト (test_sora_mode_nv12.py)
- [x] Raspberry Pi E2E テスト (test_sora_mode_raspberry_pi.py)
  - libcamera キャプチャ + V4L2 M2M エンコーダー (self-hosted runner)
- [x] メトリクス API 統計情報詳細検証
  - version / libwebrtc / environment のフォーマット検証、stats 構造検証 (test_metrics_api.py)
  - `find_stats()` / `find_all_stats()` は `test_ayame_mode.py` に定義済み
- [x] VP9 / AV1 / H264 / H265 コーデックの実動作テスト
  - NVIDIA NvCodec (H264/H265/AV1)、Intel oneVPL (VP9/AV1/H264/H265) で検証 (self-hosted runner)

未実装:
- [ ] `--insecure` / 音声処理オプションの動作テスト
  - 各機能 issue の実装が前提

### E2E テスト基盤 (`momo.py`)

- `Momo` クラス: プロセス管理 (起動/停止/クリーンアップ)
- コンテキストマネージャー (`with Momo(...) as m:`) でライフサイクル管理
- バイナリ自動検出: 環境変数 `MOMO_BINARY` → `target/debug/momo` → `target/release/momo`
- 起動確認: メトリクス API (`GET /metrics`) へのポーリングで起動完了を検出 (タイムアウト 30 秒)
- グレースフルシャットダウン: SIGTERM → 5 秒待機 → SIGKILL
- `_validate_mode_options()`: モード固有オプションの混在をプロセス起動前に検出
- `get_metrics()`: メトリクス API からデータ取得
  - `wait_stats` パラメータで特定の統計情報が出現するまでポーリング可能
  - stats の type とフィールド値でマッチング
- `wait_for_connection()`: DTLS/ICE 接続確立を stats 経由で待機
  - transport type の `dtlsState: "connected"` と `iceState: "connected"` をチェック
- ポート管理: `conftest.py` で `itertools.count(56000)` による一意なポート割り当て

### test_p2p_mode.py の詳細

| テスト名 | 検証内容 |
|---------|---------|
| `test_with_custom_arguments` | `--resolution QVGA --framerate 15 --log-level info` でプロセス起動、メトリクス version 確認 |
| `test_multiple_instances_concurrent` | 異なるメトリクスポート・HTTP ポートで 2 インスタンス同時起動、各メトリクス独立確認 |
| `test_multiple_instances_different_configs` | 同一設定 (QVGA, 15fps) で 2 インスタンス、ポート競合なしで独立動作確認 |
| `test_dynamic_instance_creation_and_cleanup` | 3 インスタンスを動的生成 → 各メトリクス取得 → 手動クリーンアップ |

### test_ayame_mode.py の詳細

| テスト名 | 検証内容 |
|---------|---------|
| `test_ayame_mode_basic` | UUID room_id で Ayame Labo (`wss://ayame-labo.shiguredo.app/signaling`) に接続、メトリクス version 確認 |
| `test_ayame_mode_with_client_id` | UUID client_id を指定して Ayame Labo に接続 |
| `test_ayame_mode_with_video_settings` | `--vp8-encoder software` を指定して起動確認 |
| `test_ayame_mode_with_audio_settings` | echo cancellation / AGC / noise suppression を全て無効化して起動確認 |
| `test_ayame_mode_with_invalid_codec` | `INVALID_CODEC` / `INVALID_AUDIO` 指定でプロセス異常終了確認 (`RuntimeError`) |

- ヘルパー関数 `find_stats()` / `find_all_stats()` 定義済みだが現テストでは未使用

### test_momo_validation.py の詳細

| テスト名 | 検証内容 |
|---------|---------|
| `test_p2p_mode_with_ayame_options_raises_error` | P2P モードで `room_id` / `client_id` 指定時に `ValueError` 発生確認 |
| `test_ayame_mode_with_p2p_options_raises_error` | Ayame モードで `document_root` 指定時に `ValueError` 発生確認 |
| `test_p2p_mode_with_ayame_direction_raises_error` | P2P モードで `direction` 指定時に `ValueError` 発生確認 |
| `test_common_options_allowed_in_all_modes` | P2P モードで `resolution` / `framerate` / `fake_capture_device` 等の共通オプション使用可能確認 |

- プロセスを起動せずに `_validate_mode_options()` の検証ロジックをテスト (ValueError はコンストラクタで発生)

### E2E テストで未カバーの領域

- P2P / Ayame モードの WebRTC 接続確立 (Sora モードは sendonly/recvonly 等でカバー済み)
- シリアルデータチャネル
- プロキシ対応
- クライアント証明書認証 (`--client-cert` / `--client-key` は実装済み、E2E テスト未作成)
- 音声処理オプション (エコーキャンセレーション等) の実動作 (CLI に渡しているが momo-rs 側で未使用)
- スクリーンキャプチャ
- Jetson の HW エンコーダー (NVIDIA NvCodec / Intel oneVPL は self-hosted runner でカバー済み)
- VideoToolbox simulcast (webrtc-rs が HWA での simulcast 未対応のため skip)
- `--insecure` の動作検証
