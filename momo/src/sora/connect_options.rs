//! Sora の connect メッセージに載せるオプションの解析
//!
//! CLI から受け取った文字列を sora_sdk の型へ変換する。
//! sora_sdk は接続オプションの値をビルド時に検証しないため、ここで検証を済ませ、
//! Sora サーバーに拒否されてから原因が分かりにくい状態に陥るのを防ぐ。

use nojson::RawJsonValue;
use sora_sdk::{ConnectDataChannel, ForwardingFilter, ForwardingFilterRule, JsonString};

use crate::error::BoxError;

/// Sora が内部的に使用する DataChannel ラベル
///
/// リアルタイムメッセージング用とは別枠でサーバーが作成するため、指定を拒否する。
const RESERVED_DATA_CHANNEL_LABELS: [&str; 5] = ["signaling", "stats", "push", "notify", "rpc"];

/// DataChannel ラベルの最大長 (`#` を含む)
const MAX_DATA_CHANNEL_LABEL_CHARS: usize = 32;

/// `--data-channel-label` のラベル一覧を、接続時に作成する DataChannel 設定へ変換する
///
/// ラベルは `#` で始まり `#` を含めて 32 文字以内、かつ Sora の内部ラベル
/// (`signaling` / `stats` / `push` / `notify` / `rpc`) と衝突しないことを要求する。
/// 順序保証・圧縮・ヘッダーなどの細部は Sora サーバー側の既定に任せるため設定しない。
pub fn build_data_channels(labels: &[String]) -> Result<Vec<ConnectDataChannel>, BoxError> {
    let mut channels = Vec::new();
    let mut seen = Vec::new();

    for label in labels {
        if !label.starts_with('#') {
            return Err(format!(
                "不正な --data-channel-label: {label} (# で始まるラベルを指定してください)"
            )
            .into());
        }
        if label.chars().count() > MAX_DATA_CHANNEL_LABEL_CHARS {
            return Err(format!(
                "不正な --data-channel-label: {label} (# を含め {MAX_DATA_CHANNEL_LABEL_CHARS} 文字以内で指定してください)"
            )
            .into());
        }
        // `#` を除いた部分が Sora の内部ラベルと衝突しないことを確認する
        let without_prefix = &label[1..];
        if RESERVED_DATA_CHANNEL_LABELS.contains(&without_prefix) {
            return Err(format!(
                "不正な --data-channel-label: {label} (Sora が内部で使用するラベルと衝突します)"
            )
            .into());
        }
        if seen.contains(label) {
            return Err(format!("重複する --data-channel-label: {label}").into());
        }
        seen.push(label.clone());

        channels.push(ConnectDataChannel {
            label: label.clone(),
            // momo のメッセージング用途に合わせ、双方向を既定とする
            direction: "sendrecv".to_string(),
            ordered: None,
            max_packet_life_time: None,
            max_retransmits: None,
            protocol: None,
            compress: None,
            header: None,
        });
    }

    Ok(channels)
}

/// `--forwarding-filter` の JSON 配列を sora_sdk の転送フィルター一覧へ変換する
///
/// 受け付ける JSON は Sora の connect メッセージの `forwarding_filters` と同じ形。
///
/// ```json
/// [
///   {
///     "name": "filter-1",
///     "priority": 1,
///     "action": "block",
///     "rules": [
///       [
///         { "field": "kind", "operator": "is_in", "values": ["video"] }
///       ]
///     ]
///   }
/// ]
/// ```
pub fn parse_forwarding_filters(text: &str) -> Result<Vec<ForwardingFilter>, BoxError> {
    let parsed = nojson::RawJson::parse(text).map_err(|e| json_error(text, e))?;
    let root = parsed.value();

    let elements = root.to_array().map_err(|e| json_error(text, e))?;
    let mut filters = Vec::new();
    for element in elements {
        filters.push(parse_forwarding_filter(text, element)?);
    }
    Ok(filters)
}

/// `--client-id` / `--bundle-id` の値を検証する
///
/// Sora の仕様で 1〜255 バイトの任意の文字列を要求する。
pub fn validate_opaque_id(value: &str, name: &str) -> Result<(), BoxError> {
    if value.is_empty() || value.len() > 255 {
        return Err(format!("不正な {name}: {value} (1〜255 バイトで指定してください)").into());
    }
    Ok(())
}

// ─── 転送フィルターの解析 ─────────────────────────────────────────────────────

/// 転送フィルター 1 件を解析する
fn parse_forwarding_filter(
    text: &str,
    value: RawJsonValue<'_, '_>,
) -> Result<ForwardingFilter, BoxError> {
    Ok(ForwardingFilter {
        name: optional_string(text, value, "name")?,
        priority: optional_integer(text, value, "priority")?,
        action: optional_enum(text, value, "action", &["block", "allow"])?,
        rules: parse_filter_rules(text, value)?,
        version: optional_string(text, value, "version")?,
        // metadata は任意の JSON 構造をそのまま保持する
        metadata: optional_member(text, value, "metadata")?
            .map(|v| JsonString::from(v.extract().into_owned())),
    })
}

/// 転送フィルターの `rules` を解析する
///
/// 内側の配列は AND 結合、外側の配列は OR 結合として Sora に解釈される。
fn parse_filter_rules(
    text: &str,
    filter: RawJsonValue<'_, '_>,
) -> Result<Vec<Vec<ForwardingFilterRule>>, BoxError> {
    let rules = required_member(text, filter, "rules")?;
    let groups = rules.to_array().map_err(|e| filter.invalid(e))?;

    let mut parsed_groups = Vec::new();
    for group in groups {
        let rules = group.to_array().map_err(|e| group.invalid(e))?;
        let mut parsed_rules = Vec::new();
        for rule in rules {
            parsed_rules.push(parse_filter_rule(text, rule)?);
        }
        parsed_groups.push(parsed_rules);
    }
    Ok(parsed_groups)
}

/// 転送フィルターのルール 1 件を解析する
fn parse_filter_rule(
    text: &str,
    value: RawJsonValue<'_, '_>,
) -> Result<ForwardingFilterRule, BoxError> {
    let field = required_enum(
        text,
        value,
        "field",
        &["connection_id", "client_id", "kind"],
    )?;
    let operator = required_enum(text, value, "operator", &["is_in", "is_not_in"])?;

    let elements = required_member(text, value, "values")?
        .to_array()
        .map_err(|e| value.invalid(e))?;
    let mut values = Vec::new();
    for element in elements {
        values.push(to_string(text, element)?);
    }

    Ok(ForwardingFilterRule {
        field,
        operator,
        values,
    })
}

// ─── JSON アクセスヘルパー ────────────────────────────────────────────────────

/// 必須メンバーを取り出す
fn required_member<'text, 'raw>(
    text: &str,
    value: RawJsonValue<'text, 'raw>,
    name: &str,
) -> Result<RawJsonValue<'text, 'raw>, BoxError> {
    let member = value.to_member(name).map_err(|e| json_error(text, e))?;
    member.required().map_err(|e| json_error(text, e))
}

/// オプショナルメンバーを取り出す
fn optional_member<'text, 'raw>(
    text: &str,
    value: RawJsonValue<'text, 'raw>,
    name: &str,
) -> Result<Option<RawJsonValue<'text, 'raw>>, BoxError> {
    let member = value.to_member(name).map_err(|e| json_error(text, e))?;
    Ok(member.optional())
}

/// 文字列のオプショナルメンバーを取り出す
fn optional_string(
    text: &str,
    value: RawJsonValue<'_, '_>,
    name: &str,
) -> Result<Option<String>, BoxError> {
    optional_member(text, value, name)?
        .map(|v| to_string(text, v))
        .transpose()
}

/// 整数のオプショナルメンバーを取り出す
fn optional_integer(
    text: &str,
    value: RawJsonValue<'_, '_>,
    name: &str,
) -> Result<Option<i32>, BoxError> {
    let Some(v) = optional_member(text, value, name)? else {
        return Ok(None);
    };
    let parsed = i32::try_from(v).map_err(|e| json_error(text, e))?;
    Ok(Some(parsed))
}

/// 文字列メンバーを変換する
fn to_string(text: &str, value: RawJsonValue<'_, '_>) -> Result<String, BoxError> {
    String::try_from(value).map_err(|e| json_error(text, e))
}

/// 列挙値のオプショナルメンバーを取り出す
fn optional_enum(
    text: &str,
    value: RawJsonValue<'_, '_>,
    name: &str,
    candidates: &[&str],
) -> Result<Option<String>, BoxError> {
    optional_member(text, value, name)?
        .map(|v| check_enum(text, v, name, candidates))
        .transpose()
}

/// 列挙値の必須メンバーを取り出す
fn required_enum(
    text: &str,
    value: RawJsonValue<'_, '_>,
    name: &str,
    candidates: &[&str],
) -> Result<String, BoxError> {
    check_enum(text, required_member(text, value, name)?, name, candidates)
}

/// 列挙値のメンバーを照合する
fn check_enum(
    text: &str,
    value: RawJsonValue<'_, '_>,
    name: &str,
    candidates: &[&str],
) -> Result<String, BoxError> {
    let parsed = to_string(text, value)?;
    if !candidates.contains(&parsed.as_str()) {
        return Err(json_error(
            text,
            value.invalid(format!(
                "`{name}` は {} のいずれかを指定してください (指定値: {parsed})",
                candidates.join(" / ")
            )),
        ));
    }
    Ok(parsed)
}

// ─── エラー処理 ───────────────────────────────────────────────────────────────

/// 行・列番号を付けたエラーメッセージを生成する
fn json_error(text: &str, error: nojson::JsonParseError) -> BoxError {
    let location = match error.get_line_and_column_numbers(text) {
        Some((line, column)) => format!(" ({line}:{column})"),
        None => String::new(),
    };
    format!("--forwarding-filter の JSON が不正です{location}: {error}").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_channel_labels_are_converted_to_sendrecv_channels() {
        // # 付きラベルが複数指定されたとき、順不同で DataChannel 設定へ変換できる
        let labels = vec!["#chat".to_string(), "#notify-extra".to_string()];
        let channels = build_data_channels(&labels).expect("正常に変換できるはず");

        assert_eq!(channels.len(), 2, "2 件の DataChannel ができるはず");
        assert_eq!(channels[0].label, "#chat", "1 件目のラベルが一致するはず");
        assert_eq!(
            channels[0].direction, "sendrecv",
            "方向は sendrecv が既定のはず"
        );
        assert!(
            channels[0].ordered.is_none(),
            "指定していない項目は None のはず"
        );
    }

    #[test]
    fn data_channel_label_rejects_invalid_form() {
        // # プレフィックスが無い・長すぎる・Sora 内部ラベルと衝突する場合はエラー
        assert!(
            build_data_channels(&["chat".to_string()]).is_err(),
            "# 無しは拒否するはず"
        );
        let too_long = format!("#{}", "a".repeat(32));
        assert!(
            build_data_channels(&[too_long]).is_err(),
            "33 文字は拒否するはず"
        );
        assert!(
            build_data_channels(&["#stats".to_string()]).is_err(),
            "Sora 内部ラベルとの衝突は拒否するはず"
        );
        assert!(
            build_data_channels(&["#chat".to_string(), "#chat".to_string()]).is_err(),
            "重複ラベルは拒否するはず"
        );
        assert!(
            build_data_channels(&[format!("#{}", "あ".repeat(31))]).is_ok(),
            "32 文字 (日本語でも文字数で判定) は受理するはず"
        );
    }

    #[test]
    fn forwarding_filters_are_parsed_from_json() {
        // Sora の connect メッセージと同じ形の JSON を sora_sdk の型へ変換できる
        let json = r#"[
            {
                "name": "block-video",
                "priority": 10,
                "action": "block",
                "version": "v2",
                "metadata": {"author": "momo"},
                "rules": [
                    [{"field": "kind", "operator": "is_in", "values": ["video"]}],
                    [{"field": "client_id", "operator": "is_not_in", "values": ["a", "b"]}]
                ]
            }
        ]"#;
        let filters = parse_forwarding_filters(json).expect("解析できるはず");

        assert_eq!(filters.len(), 1, "フィルターは 1 件のはず");
        let filter = &filters[0];
        assert_eq!(
            filter.name.as_deref(),
            Some("block-video"),
            "name 変換結果が一致しない"
        );
        assert_eq!(filter.priority, Some(10), "priority 変換結果が一致しない");
        assert_eq!(
            filter.action.as_deref(),
            Some("block"),
            "action 変換結果が一致しない"
        );
        assert_eq!(
            filter.version.as_deref(),
            Some("v2"),
            "version 変換結果が一致しない"
        );
        assert!(filter.metadata.is_some(), "metadata が保持されるはず");
        assert_eq!(filter.rules.len(), 2, "OR 結合のルール群が 2 件あるはず");
        assert_eq!(filter.rules[1].len(), 1, "2 件目のルール群は 1 要素のはず");
        assert_eq!(
            filter.rules[0][0].field, "kind",
            "rule の field が一致しない"
        );
        assert_eq!(
            filter.rules[1][0].values,
            vec!["a".to_string(), "b".to_string()],
            "rule の values が一致しない"
        );
    }

    #[test]
    fn forwarding_filter_requires_rules_and_known_enums() {
        // rules 必須・列挙値の照合・トップレベルが配列でない場合はエラー
        assert!(
            parse_forwarding_filters(r#"[{"name": "x"}]"#).is_err(),
            "rules 無しは拒否するはず"
        );
        assert!(
            parse_forwarding_filters(
                r#"[{"rules": [[{"field": "media", "operator": "is_in", "values": ["video"]}]]}]"#
            )
            .is_err(),
            "未知の field は拒否するはず"
        );
        assert!(
            parse_forwarding_filters(
                r#"[{"rules": [[{"field": "kind", "operator": "equals", "values": ["video"]}]]}]"#
            )
            .is_err(),
            "未知の operator は拒否するはず"
        );
        assert!(
            parse_forwarding_filters(r#"{"rules": []}"#).is_err(),
            "配列以外は拒否するはず"
        );
        // rules 以外はすべて省略できる
        let filters = parse_forwarding_filters(
            r#"[{"rules": [[{"field": "kind", "operator": "is_in", "values": ["audio"]}]]}]"#,
        )
        .expect("最小構成は解析できるはず");
        assert!(filters[0].name.is_none(), "name 省略は None のはず");
        assert!(filters[0].metadata.is_none(), "metadata 省略は None のはず");
    }

    #[test]
    fn opaque_id_length_is_validated() {
        // client_id / bundle_id は 1〜255 バイト
        assert!(
            validate_opaque_id("momo-client", "--client-id").is_ok(),
            "通常値は受理するはず"
        );
        assert!(
            validate_opaque_id(&"a".repeat(255), "--client-id").is_ok(),
            "255 バイトは受理するはず"
        );
        assert!(
            validate_opaque_id("", "--client-id").is_err(),
            "空文字列は拒否するはず"
        );
        assert!(
            validate_opaque_id(&"あ".repeat(86), "--bundle-id").is_err(),
            "UTF-8 換算 255 バイト超は拒否するはず"
        );
    }
}
