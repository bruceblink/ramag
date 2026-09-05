//! SQLite 行解码和声明类型映射。

use ramag_domain::entities::{ColumnKind, ColumnType, Value};
use ramag_domain::error::{DomainError, Result};
use sqlx::sqlite::{SqliteColumn, SqliteRow};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};

pub fn decode_row(row: &SqliteRow) -> Result<Vec<Value>> {
    row.columns()
        .iter()
        .map(|column| decode_column(row, column))
        .collect()
}

fn decode_column(row: &SqliteRow, column: &SqliteColumn) -> Result<Value> {
    let index = column.ordinal();
    let type_name = column.type_info().name();
    let raw = row.try_get_raw(index).map_err(|error| {
        DomainError::QueryFailed(format!(
            "读取 SQLite 列「{}」({type_name}) 原始值失败：{error}",
            column.name()
        ))
    })?;
    if raw.is_null() {
        return Ok(Value::Null);
    }

    match type_name {
        "INTEGER" => decode_as::<i64, _>(row, column, Value::Int),
        "REAL" => decode_as::<f64, _>(row, column, Value::Float),
        "TEXT" => decode_as::<String, _>(row, column, Value::Text),
        "BLOB" => decode_as::<Vec<u8>, _>(row, column, Value::Bytes),
        _ => decode_unknown(row, column),
    }
}

fn decode_as<T, F>(row: &SqliteRow, column: &SqliteColumn, convert: F) -> Result<Value>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
    F: FnOnce(T) -> Value,
{
    row.try_get::<T, _>(column.ordinal())
        .map(convert)
        .map_err(|error| {
            DomainError::QueryFailed(format!(
                "解码 SQLite 列「{}」({}) 失败：{error}",
                column.name(),
                column.type_info().name()
            ))
        })
}

fn decode_unknown(row: &SqliteRow, column: &SqliteColumn) -> Result<Value> {
    let index = column.ordinal();
    if let Ok(value) = row.try_get::<i64, _>(index) {
        return Ok(Value::Int(value));
    }
    if let Ok(value) = row.try_get::<f64, _>(index) {
        return Ok(Value::Float(value));
    }
    if let Ok(value) = row.try_get::<String, _>(index) {
        return Ok(Value::Text(value));
    }
    row.try_get::<Vec<u8>, _>(index)
        .map(Value::Bytes)
        .map_err(|error| {
            DomainError::QueryFailed(format!(
                "读取 SQLite 列「{}」({}) 失败：{error}",
                column.name(),
                column.type_info().name()
            ))
        })
}

/// SQLite 按声明类型计算 affinity，保留完整原始类型供 UI 展示。
pub fn map_column_type(raw_type: &str) -> ColumnType {
    let upper = raw_type.trim().to_ascii_uppercase();
    let kind = if upper.contains("INT") {
        ColumnKind::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        ColumnKind::Text
    } else if upper.contains("BLOB") || upper.is_empty() {
        ColumnKind::Blob
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        ColumnKind::Float
    } else if upper.contains("DEC") || upper.contains("NUM") {
        ColumnKind::Decimal
    } else if upper.contains("BOOL") {
        ColumnKind::Bool
    } else if upper.contains("DATE") || upper.contains("TIME") {
        ColumnKind::DateTime
    } else if upper.contains("JSON") {
        ColumnKind::Json
    } else {
        ColumnKind::Other
    };
    ColumnType {
        kind,
        raw_type: raw_type.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sqlite_affinity_types() {
        assert_eq!(map_column_type("INTEGER").kind, ColumnKind::Integer);
        assert_eq!(map_column_type("VARCHAR(255)").kind, ColumnKind::Text);
        assert_eq!(map_column_type("BLOB").kind, ColumnKind::Blob);
        assert_eq!(map_column_type("DOUBLE").kind, ColumnKind::Float);
        assert_eq!(map_column_type("BOOLEAN").kind, ColumnKind::Bool);
        assert_eq!(map_column_type("JSON").kind, ColumnKind::Json);
    }
}
