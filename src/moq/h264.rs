//! H.264 の NAL 変換と AVCDecoderConfigurationRecord の取り扱い
//!
//! MOQT の映像トラックは avc1 で送る。payload は 4 バイト長プレフィックス形式
//! (canonical、draft-ietf-moq-loc-04 §2.1.3)、パラメーターセットは Video Config (0x0D) に
//! AVCDecoderConfigurationRecord として載せる。LOC は draft 由来のため将来の改版で
//! 変わる可能性がある。
//!
//! エンコーダーとデコーダー (`shiguredo_openh264`) は Annex B を使うため、
//! 境界で相互変換する。

use crate::moq::error::MoqError;

/// 長プレフィックスのサイズ (AVCDecoderConfigurationRecord の lengthSizeMinusOne に対応)
pub const LENGTH_SIZE: usize = 4;

/// AVCDecoderConfigurationRecord の内容
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcDecoderConfig {
    /// SPS の NAL 本体 (スタートコードなし)
    pub sps: Vec<Vec<u8>>,
    /// PPS の NAL 本体 (スタートコードなし)
    pub pps: Vec<Vec<u8>>,
    /// 長プレフィックスのバイト数
    pub length_size: usize,
}

/// SPS / PPS から AVCDecoderConfigurationRecord を組み立てる
///
/// 根拠: ISO/IEC 14496-15 §5.2.4.1.1。High profile 系では拡張フィールドを追加する。
/// (将来の改訂でフィールド構成が変更される可能性がある)
pub fn build_avc_decoder_config_record(
    sps_list: &[Vec<u8>],
    pps_list: &[Vec<u8>],
) -> Result<Vec<u8>, MoqError> {
    let sps = sps_list.first().ok_or_else(|| {
        MoqError::Media("AVCDecoderConfigurationRecord requires an SPS".to_owned())
    })?;
    if sps.len() < 4 {
        return Err(MoqError::Media(
            "H.264 SPS is too short to read profile/level".to_owned(),
        ));
    }
    if sps_list.len() > 0x1F {
        return Err(MoqError::Media(format!(
            "too many SPS NAL units: {}",
            sps_list.len()
        )));
    }
    if pps_list.len() > 0xFF {
        return Err(MoqError::Media(format!(
            "too many PPS NAL units: {}",
            pps_list.len()
        )));
    }
    for nal in sps_list.iter().chain(pps_list.iter()) {
        if nal.len() > u16::MAX as usize {
            return Err(MoqError::Media(format!(
                "H.264 parameter set NAL is too long: {}",
                nal.len()
            )));
        }
    }

    let profile_idc = sps[1];
    let profile_compatibility = sps[2];
    let level_idc = sps[3];

    let mut buf = Vec::with_capacity(64);
    buf.push(1); // configurationVersion
    buf.push(profile_idc); // AVCProfileIndication
    buf.push(profile_compatibility); // profile_compatibility
    buf.push(level_idc); // AVCLevelIndication
    // 6 bits reserved ('111111') | 2 bits lengthSizeMinusOne
    buf.push(0xFC | ((LENGTH_SIZE as u8) - 1));
    // 3 bits reserved ('111') | 5 bits numOfSequenceParameterSets
    buf.push(0xE0 | (sps_list.len() as u8));
    for sps in sps_list {
        buf.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        buf.extend_from_slice(sps);
    }
    buf.push(pps_list.len() as u8);
    for pps in pps_list {
        buf.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        buf.extend_from_slice(pps);
    }

    // High / High10 / High 4:2:2 / High 4:4:4 profile はチャンネル拡張フィールドを追加する
    // (ISO/IEC 14496-15 §5.2.4.1.1)
    if matches!(profile_idc, 100 | 110 | 122 | 144) {
        // 6 bits reserved ('111111') | 2 bits chroma_format (1 = 4:2:0)
        buf.push(0xFC | 0x01);
        // 5 bits reserved ('11111') | 3 bits bit_depth_luma_minus8 (0 = 8-bit)
        buf.push(0xF8);
        // 5 bits reserved ('11111') | 3 bits bit_depth_chroma_minus8 (0 = 8-bit)
        buf.push(0xF8);
        // numOfSequenceParameterSetExt = 0
        buf.push(0x00);
    }
    Ok(buf)
}

/// AVCDecoderConfigurationRecord をパースする
///
/// 根拠: ISO/IEC 14496-15 §5.2.4.1.1。(将来の改訂でフィールド構成が変更される可能性がある)
pub fn parse_avc_decoder_config_record(data: &[u8]) -> Result<AvcDecoderConfig, MoqError> {
    if data.len() < 7 {
        return Err(MoqError::Media(format!(
            "AVCDecoderConfigurationRecord is too short: {}",
            data.len()
        )));
    }
    if data[0] != 1 {
        return Err(MoqError::Media(format!(
            "unsupported AVCDecoderConfigurationRecord version: {}",
            data[0]
        )));
    }
    let length_size = ((data[4] & 0x03) as usize) + 1;
    let num_sps = (data[5] & 0x1F) as usize;
    let mut offset = 6;
    let mut sps = Vec::with_capacity(num_sps);
    for _ in 0..num_sps {
        if offset + 2 > data.len() {
            return Err(MoqError::Media(
                "AVCDecoderConfigurationRecord is truncated in SPS list".to_owned(),
            ));
        }
        let length = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + length > data.len() {
            return Err(MoqError::Media(
                "AVCDecoderConfigurationRecord is truncated in SPS".to_owned(),
            ));
        }
        sps.push(data[offset..offset + length].to_vec());
        offset += length;
    }
    if offset >= data.len() {
        return Err(MoqError::Media(
            "AVCDecoderConfigurationRecord has no PPS count".to_owned(),
        ));
    }
    let num_pps = data[offset] as usize;
    offset += 1;
    let mut pps = Vec::with_capacity(num_pps);
    for _ in 0..num_pps {
        if offset + 2 > data.len() {
            return Err(MoqError::Media(
                "AVCDecoderConfigurationRecord is truncated in PPS list".to_owned(),
            ));
        }
        let length = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + length > data.len() {
            return Err(MoqError::Media(
                "AVCDecoderConfigurationRecord is truncated in PPS".to_owned(),
            ));
        }
        pps.push(data[offset..offset + length].to_vec());
        offset += length;
    }
    Ok(AvcDecoderConfig {
        sps,
        pps,
        length_size,
    })
}

/// SPS の先頭 3 バイトから `avc1.PPCCLL` 形式の codec 文字列を組み立てる
///
/// 根拠: ISO/IEC 14496-10 §7.3.2.1。NAL ユニットヘッダー 1 バイトを除いた本体の
/// 先頭 3 バイトが profile_idc / constraint_flags / level_idc。
/// (将来の改訂で SPS のレイアウトが変更される可能性がある)
pub fn avc_codec_string_from_sps(sps: &[u8]) -> Result<String, MoqError> {
    if sps.len() < 4 {
        return Err(MoqError::Media(
            "H.264 SPS is too short to build a codec string".to_owned(),
        ));
    }
    Ok(format!("avc1.{:02X}{:02X}{:02X}", sps[1], sps[2], sps[3]))
}

/// Annex B のバイト列を NAL 本体のリストに分割する
///
/// スタートコードは 3 バイト (`0x00 0x00 0x01`) と 4 バイト (`0x00 0x00 0x00 0x01`) の
/// 両方を扱う。末尾のゼロ詰めは NAL に含めない。
pub fn split_annex_b(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nals = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut index = 0;
    while index < data.len() {
        let start_length = if data[index..].starts_with(&[0, 0, 1]) {
            Some(3)
        } else if data[index..].starts_with(&[0, 0, 0, 1]) {
            Some(4)
        } else {
            None
        };
        if let Some(start_length) = start_length {
            if let Some(start) = current_start {
                push_nal(&mut nals, &data[start..index]);
            }
            index += start_length;
            current_start = Some(index);
            continue;
        }
        index += 1;
    }
    if let Some(start) = current_start {
        push_nal(&mut nals, &data[start..]);
    }
    nals
}

/// NAL 本体を追加する
///
/// Annex B の NAL ユニット末尾には trailing_zero_8bits が付きうるため、末尾の
/// ゼロバイトは NAL に含めない (ISO/IEC 14496-10 Annex B)。空の NAL は無視する。
fn push_nal(nals: &mut Vec<Vec<u8>>, nal: &[u8]) {
    let end = match nal.iter().rposition(|byte| *byte != 0) {
        Some(index) => index + 1,
        None => return,
    };
    nals.push(nal[..end].to_vec());
}

/// Annex B を長プレフィックス形式に変換する
///
/// プレフィックスのバイト数は [`LENGTH_SIZE`] 固定。NAL が無い場合は空を返す。
pub fn annex_b_to_length_prefixed(data: &[u8]) -> Result<Vec<u8>, MoqError> {
    let nals = split_annex_b(data);
    let mut buf = Vec::with_capacity(data.len());
    for nal in &nals {
        if nal.len() > u32::MAX as usize {
            return Err(MoqError::Media(format!(
                "H.264 NAL is too long: {}",
                nal.len()
            )));
        }
        buf.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        buf.extend_from_slice(nal);
    }
    Ok(buf)
}

/// 長プレフィックス形式を Annex B に変換する
pub fn length_prefixed_to_annex_b(data: &[u8], length_size: usize) -> Result<Vec<u8>, MoqError> {
    if !(1..=4).contains(&length_size) {
        return Err(MoqError::Media(format!(
            "invalid NAL length size: {length_size}"
        )));
    }
    let mut buf = Vec::with_capacity(data.len());
    let mut offset = 0;
    while offset < data.len() {
        if offset + length_size > data.len() {
            return Err(MoqError::Media(
                "length-prefixed H.264 payload is truncated in a length field".to_owned(),
            ));
        }
        let mut length = 0usize;
        for byte in &data[offset..offset + length_size] {
            length = (length << 8) | (*byte as usize);
        }
        offset += length_size;
        if offset + length > data.len() {
            return Err(MoqError::Media(
                "length-prefixed H.264 payload is truncated in a NAL".to_owned(),
            ));
        }
        buf.extend_from_slice(&[0, 0, 0, 1]);
        buf.extend_from_slice(&data[offset..offset + length]);
        offset += length;
    }
    Ok(buf)
}

/// NAL のリストを Annex B のバイト列に変換する
///
/// Video Config の SPS / PPS を payload の前へ付けるときに使う。
pub fn nals_to_annex_b(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut buf = Vec::new();
    for nal in nals {
        buf.extend_from_slice(&[0, 0, 0, 1]);
        buf.extend_from_slice(nal);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の SPS (Baseline, level 3.1)
    fn sps() -> Vec<u8> {
        vec![0x67, 0x42, 0xC0, 0x1F, 0x8C, 0x8D, 0x40]
    }

    /// テスト用の PPS
    fn pps() -> Vec<u8> {
        vec![0x68, 0xCE, 0x3C, 0x80]
    }

    /// avcC を組み立ててパースし直すと一致すること
    #[test]
    fn test_avc_decoder_config_record_round_trip() {
        let sps_list = vec![sps()];
        let pps_list = vec![pps()];
        let record =
            build_avc_decoder_config_record(&sps_list, &pps_list).expect("組み立てに成功すること");
        let parsed = parse_avc_decoder_config_record(&record).expect("パースに成功すること");
        assert_eq!(parsed.sps, sps_list);
        assert_eq!(parsed.pps, pps_list);
        assert_eq!(parsed.length_size, LENGTH_SIZE);
    }

    /// Baseline profile では拡張フィールドを付けないこと
    #[test]
    fn test_avc_decoder_config_record_baseline_has_no_extension() {
        let record =
            build_avc_decoder_config_record(&[sps()], &[pps()]).expect("組み立てに成功すること");
        // 固定部 6 + SPS (2 + 7) + PPS 数 1 + PPS (2 + 4) = 22
        assert_eq!(record.len(), 22);
    }

    /// 空の SPS リストを拒否すること
    #[test]
    fn test_avc_decoder_config_record_rejects_empty_sps() {
        assert!(build_avc_decoder_config_record(&[], &[pps()]).is_err());
    }

    /// 古いバージョンの avcC を拒否すること
    #[test]
    fn test_parse_rejects_unsupported_version() {
        let mut record =
            build_avc_decoder_config_record(&[sps()], &[pps()]).expect("組み立てに成功すること");
        record[0] = 2;
        assert!(parse_avc_decoder_config_record(&record).is_err());
    }

    /// 途中で切れた avcC を拒否すること
    #[test]
    fn test_parse_rejects_truncated_record() {
        let record =
            build_avc_decoder_config_record(&[sps()], &[pps()]).expect("組み立てに成功すること");
        assert!(parse_avc_decoder_config_record(&record[..record.len() - 3]).is_err());
    }

    /// SPS から codec 文字列を組み立てられること
    #[test]
    fn test_avc_codec_string_from_sps() {
        assert_eq!(
            avc_codec_string_from_sps(&sps()).expect("組み立てに成功すること"),
            "avc1.42C01F"
        );
    }

    /// 短い SPS を拒否すること
    #[test]
    fn test_avc_codec_string_rejects_short_sps() {
        assert!(avc_codec_string_from_sps(&[0x67]).is_err());
    }

    /// 4 バイトのスタートコードを分割できること
    #[test]
    fn test_split_annex_b_with_four_byte_start_code() {
        let data = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE];
        let nals = split_annex_b(&data);
        assert_eq!(nals, vec![vec![0x67, 0x42], vec![0x68, 0xCE]]);
    }

    /// 3 バイトのスタートコードを分割できること
    #[test]
    fn test_split_annex_b_with_three_byte_start_code() {
        let data = [0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xCE];
        let nals = split_annex_b(&data);
        assert_eq!(nals, vec![vec![0x67, 0x42], vec![0x68, 0xCE]]);
    }

    /// 末尾のゼロ詰めを NAL に含めないこと
    #[test]
    fn test_split_annex_b_ignores_trailing_zeros() {
        let data = [0, 0, 0, 1, 0x65, 0x80, 0x00, 0x00];
        let nals = split_annex_b(&data);
        assert_eq!(nals, vec![vec![0x65, 0x80]]);
    }

    /// Annex B と長プレフィックスを相互変換できること
    #[test]
    fn test_annex_b_and_length_prefixed_round_trip() {
        let data = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE];
        let length_prefixed = annex_b_to_length_prefixed(&data).expect("変換に成功すること");
        assert_eq!(
            length_prefixed,
            vec![0, 0, 0, 2, 0x67, 0x42, 0, 0, 0, 2, 0x68, 0xCE]
        );
        let annex_b =
            length_prefixed_to_annex_b(&length_prefixed, LENGTH_SIZE).expect("変換に成功すること");
        assert_eq!(annex_b, data);
    }

    /// 空の入力で空を返すこと
    #[test]
    fn test_annex_b_to_length_prefixed_empty() {
        assert!(
            annex_b_to_length_prefixed(&[])
                .expect("変換に成功すること")
                .is_empty()
        );
    }

    /// 長さフィールドの途中で切れた入力を拒否すること
    #[test]
    fn test_length_prefixed_rejects_truncated_length() {
        assert!(length_prefixed_to_annex_b(&[0, 0], LENGTH_SIZE).is_err());
    }

    /// NAL の途中で切れた入力を拒否すること
    #[test]
    fn test_length_prefixed_rejects_truncated_nal() {
        assert!(length_prefixed_to_annex_b(&[0, 0, 0, 4, 0x65], LENGTH_SIZE).is_err());
    }

    /// NAL のリストを Annex B に変換できること
    #[test]
    fn test_nals_to_annex_b() {
        assert_eq!(
            nals_to_annex_b(&[vec![0x67, 0x42], vec![0x68, 0xCE]]),
            vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE]
        );
    }
}
