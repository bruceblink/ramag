//! 结果单元格的显示状态与复制格式。

use ramag_domain::entities::{DriverKind, Value};

/// 结果单元格可复制的四种表示方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellCopyFormat {
    Text,
    Csv,
    Json,
    Sql,
}

impl CellCopyFormat {
    pub(crate) const ALL: [Self; 4] = [Self::Text, Self::Csv, Self::Json, Self::Sql];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Text => "文本",
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::Sql => "SQL 值",
        }
    }
}

/// 为表格和其他结果视图提供一致的单元格预览；16 字节二进制可按设置显示为 UUID 或原始 hex。
pub(crate) fn display_cell_value(
    value: Option<&Value>,
    max_chars: usize,
    display_binary_16_as_uuid: bool,
) -> String {
    match value {
        None => "缺失".to_string(),
        Some(Value::Null) => "NULL".to_string(),
        Some(Value::Text(text)) if text.is_empty() => "\"\"".to_string(),
        Some(value @ Value::Bytes(bytes)) if bytes.len() == 16 && display_binary_16_as_uuid => {
            value.display_preview(max_chars)
        }
        Some(Value::Bytes(bytes)) if bytes.len() == 16 => hex_preview(bytes, max_chars),
        Some(Value::Bytes(bytes)) => format!("[二进制 · {} bytes]", bytes.len()),
        Some(value) => value.display_preview(max_chars),
    }
}

fn hex_preview(bytes: &[u8], max_chars: usize) -> String {
    let mut hex = hex::encode(bytes);
    if hex.len() <= max_chars {
        return hex;
    }
    if max_chars == 0 {
        return String::new();
    }
    hex.truncate(max_chars.saturating_sub(1));
    hex.push('…');
    hex
}

/// 返回结果面板上展示给用户的值状态。
pub(crate) fn cell_status(value: Option<&Value>) -> &'static str {
    match value {
        None => "字段缺失",
        Some(Value::Null) => "NULL",
        Some(Value::Text(text)) if text.is_empty() => "空字符串",
        Some(Value::Bytes(_)) => "二进制",
        Some(Value::Json(_)) => "JSON",
        Some(Value::Bool(_)) => "布尔值",
        Some(Value::Int(_)) => "整数",
        Some(Value::Float(_)) => "浮点数",
        Some(Value::DateTime(_)) => "时间值",
        Some(Value::Text(_)) => "文本",
    }
}

/// 返回查看器中的负载大小提示，不序列化整个 JSON 或复制大字段。
pub(crate) fn cell_size_summary(value: Option<&Value>) -> String {
    match value {
        None => "没有值".to_string(),
        Some(Value::Null) => "无负载".to_string(),
        Some(Value::Text(text)) => format!("{} bytes", text.len()),
        Some(Value::Bytes(bytes)) => format!("{} bytes", bytes.len()),
        Some(Value::Json(_)) => "结构化值".to_string(),
        Some(Value::Bool(_)) => "布尔值".to_string(),
        Some(Value::Int(_)) => "整数".to_string(),
        Some(Value::Float(_)) => "浮点数".to_string(),
        Some(Value::DateTime(_)) => "时间值".to_string(),
    }
}

/// 按用户选择的格式生成单个单元格内容；不会遍历同一结果中的其他行。
pub(crate) fn format_cell_value(
    value: Option<&Value>,
    format: CellCopyFormat,
    driver: DriverKind,
) -> String {
    match format {
        CellCopyFormat::Text => value.map_or_else(String::new, Value::to_clipboard_string),
        CellCopyFormat::Csv => format_csv_value(value),
        CellCopyFormat::Json => {
            serde_json::to_string(&json_value(value)).unwrap_or_else(|_| "null".to_string())
        }
        CellCopyFormat::Sql => value.map_or_else(
            || "NULL".to_string(),
            |value| value.to_sql_literal_for(driver),
        ),
    }
}

/// 将一个单元格编码为合法 CSV 字段；空字符串使用 `""` 保留与 NULL 的区别。
fn format_csv_value(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if matches!(value, Value::Null) {
        return String::new();
    }
    let raw = value.to_clipboard_string();
    if raw.is_empty() {
        return "\"\"".to_string();
    }
    if raw.contains([',', '"', '\n', '\r']) {
        return format!("\"{}\"", raw.replace('"', "\"\""));
    }
    raw
}

/// 把领域值映射为 JSON 标量；非有限浮点数降级为字符串，避免复制动作失败。
fn json_value(value: Option<&Value>) -> serde_json::Value {
    match value {
        None | Some(Value::Null) => serde_json::Value::Null,
        Some(Value::Bool(value)) => serde_json::Value::Bool(*value),
        Some(Value::Int(value)) => serde_json::Value::Number((*value).into()),
        Some(Value::Float(value)) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(value.to_string())),
        Some(Value::Text(value)) => serde_json::Value::String(value.clone()),
        Some(Value::Bytes(value)) => serde_json::Value::String(hex::encode(value)),
        Some(Value::DateTime(value)) => serde_json::Value::String(value.to_rfc3339()),
        Some(Value::Json(value)) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::DriverKind;

    #[test]
    fn display_state_distinguishes_null_empty_and_binary() {
        assert_eq!(display_cell_value(Some(&Value::Null), 20, true), "NULL");
        assert_eq!(
            display_cell_value(Some(&Value::Text(String::new())), 20, true),
            "\"\""
        );
        assert_eq!(
            display_cell_value(Some(&Value::Bytes(vec![1, 2, 3])), 20, true),
            "[二进制 · 3 bytes]"
        );
        assert_eq!(
            display_cell_value(
                Some(&Value::Bytes(vec![
                    0x01, 0x9f, 0xeb, 0x23, 0xf4, 0xb0, 0x71, 0x73, 0x93, 0x7d, 0x07, 0x2d, 0x15,
                    0x24, 0x8b, 0x70,
                ])),
                60,
                true
            ),
            "019feb23-f4b0-7173-937d-072d15248b70"
        );
        assert_eq!(
            display_cell_value(
                Some(&Value::Bytes(vec![
                    0x01, 0x9f, 0xeb, 0x23, 0xf4, 0xb0, 0x71, 0x73, 0x93, 0x7d, 0x07, 0x2d, 0x15,
                    0x24, 0x8b, 0x70,
                ])),
                60,
                false
            ),
            "019feb23f4b07173937d072d15248b70"
        );
        assert_eq!(
            display_cell_value(
                Some(&Value::Bytes(vec![
                    0x01, 0x9f, 0xeb, 0x23, 0xf4, 0xb0, 0x71, 0x73, 0x93, 0x7d, 0x07, 0x2d, 0x15,
                    0x24, 0x8b, 0x70,
                ])),
                9,
                false
            ),
            "019feb23…"
        );
        assert_eq!(cell_status(None), "字段缺失");
        assert_eq!(cell_status(Some(&Value::Text(String::new()))), "空字符串");
    }

    #[test]
    fn copy_formats_keep_value_semantics() {
        let value = Value::Text("O'Reilly, \"book\"".into());
        assert_eq!(
            format_cell_value(Some(&value), CellCopyFormat::Text, DriverKind::Mysql),
            "O'Reilly, \"book\""
        );
        assert_eq!(
            format_cell_value(Some(&value), CellCopyFormat::Csv, DriverKind::Mysql),
            "\"O'Reilly, \"\"book\"\"\""
        );
        assert_eq!(
            format_cell_value(Some(&value), CellCopyFormat::Json, DriverKind::Mysql),
            "\"O'Reilly, \\\"book\\\"\""
        );
        assert_eq!(
            format_cell_value(Some(&value), CellCopyFormat::Sql, DriverKind::Mysql),
            "'O''Reilly, \"book\"'"
        );
        assert_eq!(
            format_cell_value(Some(&Value::Null), CellCopyFormat::Json, DriverKind::Mysql),
            "null"
        );
    }

    #[test]
    fn csv_distinguishes_missing_or_null_from_empty_text() {
        assert_eq!(
            format_cell_value(None, CellCopyFormat::Csv, DriverKind::Mysql),
            ""
        );
        assert_eq!(
            format_cell_value(Some(&Value::Null), CellCopyFormat::Csv, DriverKind::Mysql),
            ""
        );
        assert_eq!(
            format_cell_value(
                Some(&Value::Text(String::new())),
                CellCopyFormat::Csv,
                DriverKind::Mysql
            ),
            "\"\""
        );
    }

    #[test]
    fn non_finite_float_json_is_safe_to_copy() {
        let copied = format_cell_value(
            Some(&Value::Float(f64::NAN)),
            CellCopyFormat::Json,
            DriverKind::Mysql,
        );
        assert_eq!(copied, "\"NaN\"");
    }
}
