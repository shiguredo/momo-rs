# PCMU / PCMA 音声コーデック対応

- Created: 2026-05-04
- Completed: {YYYY-MM-DD}
- Branch: feature/add-pcmu-pcma-codec
- Polished: {YYYY-MM-DD}

## 目的

Ayame モードで OPUS 以外に PCMU / PCMA 音声コーデックをサポートする。

## 現状

- `src/main.rs` の `run_ayame` で `--audio-codec-type` の値として PCMU / PCMA がバリデーション可能
- ただし Ayame の Transceiver `set_codec_preferences` には反映されない
- Sora モードは sora_sdk が音声コーデックを管理しているため対象外

## momo の対応状況

- `src/util.cpp` の `--audio-codec-type` の選択肢として OPUS / PCMU / PCMA を許可
- SetCodecPreferences で PCMU / PCMA を primary に指定すると SDP に反映される

## 必要な検討

- shiguredo_webrtc の `get_rtp_sender_capabilities(MediaType::Audio)` で PCMU / PCMA が返るか確認する
- 返らない場合: shiguredo_webrtc 側の C API 拡張が必要
- 返る場合: AyameConfig の audio_codec_type 経路で `set_codec_preferences` を呼ぶだけで対応可能

## pending 理由

shiguredo_webrtc が PCMU / PCMA をエンコーダ/デコーダとして扱えるかが未確認。音声コーデックの追加は webrtc-rs (libwebrtc 内蔵の G.711 実装) が利用可能になっているか次第のため、調査・判断が必要。

## 関連

- Ayame モードで `--video-codec-type` が無視される問題: 映像側の同種問題、既に解決済み
- 同じ `set_codec_preferences` の仕組みで音声側も対応可能

## 参考

- momo-cpp の `--audio-codec-type` 実装 (`src/util.cpp`)
- `src/main.rs` の `run_ayame` の audio-codec-type バリデーション
