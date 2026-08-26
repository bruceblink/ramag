use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use ramag_domain::entities::{QueryResult, Row, Value};

use super::{
    ColumnPair, MAX_ROW_FIELDS, MAX_ROW_LINES, MAX_ROW_PREVIEW_CHARS, MAX_VALUE_PREVIEW_CHARS,
    ResultDiffKind, ResultDiffLine, RowKey,
};

pub(super) fn identity_key(
    row: Option<&Row>,
    columns: &[ColumnPair],
    source_side: bool,
) -> Option<RowKey> {
    let row = row?;
    let mut values = Vec::with_capacity(columns.len());
    for column in columns {
        let index = if source_side {
            column.source
        } else {
            column.target
        };
        let value = row.values.get(index)?;
        if matches!(value, Value::Null) {
            return None;
        }
        values.push(value_hash(Some(value)));
    }
    Some(RowKey(values))
}

pub(super) fn content_key(row: Option<&Row>, columns: &[ColumnPair], source_side: bool) -> RowKey {
    RowKey(
        columns
            .iter()
            .map(|column| {
                let index = if source_side {
                    column.source
                } else {
                    column.target
                };
                value_hash(row.and_then(|row| row.values.get(index)))
            })
            .collect(),
    )
}

pub(super) fn identity_values_equal(
    source: Option<&Row>,
    target: Option<&Row>,
    columns: &[ColumnPair],
) -> bool {
    let (Some(source), Some(target)) = (source, target) else {
        return false;
    };
    columns.iter().all(|column| {
        let source = source.values.get(column.source);
        let target = target.values.get(column.target);
        !matches!(
            (source, target),
            (Some(Value::Null), _) | (_, Some(Value::Null))
        ) && values_equal(source, target)
    })
}

pub(super) fn rows_equal(
    source: Option<&Row>,
    target: Option<&Row>,
    columns: &[ColumnPair],
) -> bool {
    let (Some(source), Some(target)) = (source, target) else {
        return false;
    };
    columns.iter().all(|column| {
        values_equal(
            source.values.get(column.source),
            target.values.get(column.target),
        )
    })
}

fn values_equal(source: Option<&Value>, target: Option<&Value>) -> bool {
    match (source, target) {
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::Bool(source)), Some(Value::Bool(target))) => source == target,
        (Some(Value::Int(source)), Some(Value::Int(target))) => source == target,
        (Some(Value::Float(source)), Some(Value::Float(target))) => {
            source.to_bits() == target.to_bits()
        }
        (Some(Value::Text(source)), Some(Value::Text(target))) => source == target,
        (Some(Value::Bytes(source)), Some(Value::Bytes(target))) => source == target,
        (Some(Value::DateTime(source)), Some(Value::DateTime(target))) => source == target,
        (Some(Value::Json(source)), Some(Value::Json(target))) => source == target,
        (None, None) => true,
        _ => false,
    }
}

fn value_hash(value: Option<&Value>) -> u64 {
    let mut hasher = DefaultHasher::new();
    match value {
        None => 0u8.hash(&mut hasher),
        Some(value) => {
            1u8.hash(&mut hasher);
            hash_value(value, &mut hasher);
        }
    }
    hasher.finish()
}

fn hash_value<H: Hasher>(value: &Value, hasher: &mut H) {
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(value) => {
            1u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Int(value) => {
            2u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Float(value) => {
            3u8.hash(hasher);
            value.to_bits().hash(hasher);
        }
        Value::Text(value) => {
            4u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Bytes(value) => {
            5u8.hash(hasher);
            value.hash(hasher);
        }
        Value::DateTime(value) => {
            6u8.hash(hasher);
            value.timestamp().hash(hasher);
            value.timestamp_subsec_nanos().hash(hasher);
        }
        Value::Json(value) => {
            7u8.hash(hasher);
            hash_json(value, hasher);
        }
    }
}

fn hash_json<H: Hasher>(value: &serde_json::Value, hasher: &mut H) {
    match value {
        serde_json::Value::Null => 0u8.hash(hasher),
        serde_json::Value::Bool(value) => {
            1u8.hash(hasher);
            value.hash(hasher);
        }
        serde_json::Value::Number(value) => {
            2u8.hash(hasher);
            value.to_string().hash(hasher);
        }
        serde_json::Value::String(value) => {
            3u8.hash(hasher);
            value.hash(hasher);
        }
        serde_json::Value::Array(values) => {
            4u8.hash(hasher);
            values.len().hash(hasher);
            for value in values {
                hash_json(value, hasher);
            }
        }
        serde_json::Value::Object(values) => {
            5u8.hash(hasher);
            let mut entries = Vec::with_capacity(values.len());
            for (key, value) in values {
                let mut entry_hasher = DefaultHasher::new();
                key.hash(&mut entry_hasher);
                hash_json(value, &mut entry_hasher);
                entries.push(entry_hasher.finish());
            }
            entries.sort_unstable();
            entries.hash(hasher);
        }
    }
}

pub(super) fn format_column(result: &QueryResult, index: usize) -> String {
    let name = result
        .columns
        .get(index)
        .map(|name| crate::views::inline_text_preview(name, 96))
        .unwrap_or_else(|| "<缺失列名>".to_string());
    let data_type = result
        .column_types
        .get(index)
        .filter(|data_type| !data_type.trim().is_empty())
        .map(|data_type| crate::views::inline_text_preview(data_type, 80))
        .unwrap_or_else(|| "未知类型".to_string());
    format!("#{} {} : {}", index + 1, name, data_type)
}

pub(super) fn format_row(result: &QueryResult, index: usize) -> String {
    let Some(row) = result.rows.get(index) else {
        return format!("第 {} 行 | <行已不存在>", index + 1);
    };
    let field_count = result.columns.len().min(MAX_ROW_FIELDS);
    let mut fields = Vec::with_capacity(field_count + 1);
    for column_index in 0..field_count {
        let name = result
            .columns
            .get(column_index)
            .map(|name| crate::views::inline_text_preview(name, 64))
            .unwrap_or_else(|| format!("列 {}", column_index + 1));
        let value = row
            .values
            .get(column_index)
            .map(|value| value.display_preview(MAX_VALUE_PREVIEW_CHARS))
            .unwrap_or_else(|| "<缺失>".to_string());
        fields.push(format!("{name}={value}"));
    }
    if result.columns.len() > field_count {
        fields.push(format!("…其余 {} 列", result.columns.len() - field_count));
    }
    crate::views::inline_text_preview(
        &format!("第 {} 行 | {}", index + 1, fields.join(" · ")),
        MAX_ROW_PREVIEW_CHARS,
    )
}

pub(super) fn push_row_pair(
    lines: &mut Vec<ResultDiffLine>,
    omitted_lines: &mut usize,
    source: &QueryResult,
    target: &QueryResult,
    source_index: usize,
    target_index: usize,
) {
    push_line(
        lines,
        MAX_ROW_LINES,
        omitted_lines,
        ResultDiffKind::Removed,
        format_row(source, source_index),
    );
    push_line(
        lines,
        MAX_ROW_LINES,
        omitted_lines,
        ResultDiffKind::Added,
        format_row(target, target_index),
    );
}

pub(super) fn push_line(
    lines: &mut Vec<ResultDiffLine>,
    limit: usize,
    omitted_lines: &mut usize,
    kind: ResultDiffKind,
    text: String,
) {
    if lines.len() < limit {
        lines.push(ResultDiffLine { kind, text });
    } else {
        *omitted_lines = omitted_lines.saturating_add(1);
    }
}
