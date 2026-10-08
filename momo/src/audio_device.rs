//! 音声デバイス指定文字列の解決
//!
//! CLI の `--audio-input-device` / `--audio-output-device` で指定された
//! デバイス文字列を、`--list-devices` で示されるデバイス一覧と照合して
//! デバイスの unique_id に解決する。

use shiguredo_audio_device::{AudioDeviceList, AudioDeviceType};

/// デバイス指定文字列を列挙して unique_id に解決する
///
/// 指定方法は次の 3 つのいずれか:
/// - インデックス番号 (`--list-devices` の表示順、0 から)
/// - デバイス名
/// - unique_id
///
/// `spec` が `None` の場合はデバイス一覧を列挙せずに `Ok(None)` を返す。
/// デバイスが見つからない場合はエラーを返し、呼び出し側で起動を中止する。
pub fn resolve_audio_device_id(
    spec: Option<&str>,
    direction: AudioDeviceType,
) -> Result<Option<String>, String> {
    let Some(spec) = spec else {
        return Ok(None);
    };

    let list = match direction {
        AudioDeviceType::Input => AudioDeviceList::enumerate_input(),
        AudioDeviceType::Output => AudioDeviceList::enumerate_output(),
    }
    .map_err(|e| format!("音声デバイスの列挙に失敗: {e}"))?;

    let devices: Vec<(String, String)> = list
        .as_slice()
        .iter()
        .filter_map(|d| Some((d.name().ok()?, d.unique_id().ok()?)))
        .collect();

    match resolve_audio_device_in_list(spec, &devices) {
        Ok(unique_id) => Ok(Some(unique_id)),
        Err(e) => {
            // 解決できなかった場合も名前や ID をライブラリに渡すと
            // デフォルトデバイスが選ばれてしまうため、エラーで気付かせる
            Err(format!("{e} (`--list-devices` で確認してください)"))
        }
    }
}

/// 与えられたデバイス一覧から指定文字列を解決する
///
/// インデックス番号、デバイス名、unique_id のいずれかを解決できる。
/// 解決できない場合はエラー文字列を返す。
pub fn resolve_audio_device_in_list(
    spec: &str,
    devices: &[(String, String)],
) -> Result<String, String> {
    if let Ok(index) = spec.parse::<usize>() {
        return devices
            .get(index)
            .map(|(_, unique_id)| unique_id.clone())
            .ok_or_else(|| format!("指定されたインデックスは存在しません: {index}"));
    }

    devices
        .iter()
        .find(|(name, unique_id)| name == spec || unique_id == spec)
        .map(|(_, unique_id)| unique_id.clone())
        .ok_or_else(|| format!("音声デバイスが見つかりませんでした: '{spec}'"))
}
