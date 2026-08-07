//! MySQL 行解码；DECIMAL 使用文本保留精度，失败时尝试文本兜底。

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use ramag_domain::entities::{ColumnKind, ColumnType, Value};
use ramag_domain::error::{DomainError, Result};
use sqlx::Column as _;
use sqlx::TypeInfo as _;
use sqlx::mysql::{MySqlColumn, MySqlRow};
use sqlx::types::BigDecimal;
use sqlx::{Row, ValueRef};

pub fn decode_row(row: &MySqlRow) -> Result<Vec<Value>> {
    row.columns()
        .iter()
        .map(|col| decode_column(row, col))
        .collect()
}

fn decode_column(row: &MySqlRow, col: &MySqlColumn) -> Result<Value> {
    let type_name = col.type_info().name();
    let idx = col.ordinal();

    let raw = row.try_get_raw(idx).map_err(|error| {
        DomainError::QueryFailed(format!(
            "读取列「{}」({type_name}) 原始值失败：{error}",
            col.name()
        ))
    })?;
    if raw.is_null() {
        return Ok(Value::Null);
    }

    match type_name {
        "BOOLEAN" => decode_as::<bool, _>(row, col, Value::Bool),

        "TINYINT" => decode_int::<i8>(row, idx),
        "TINYINT UNSIGNED" => decode_int::<u8>(row, idx),
        "SMALLINT" => decode_int::<i16>(row, idx),
        "SMALLINT UNSIGNED" => decode_int::<u16>(row, idx),
        "MEDIUMINT" => decode_int::<i32>(row, idx),
        "MEDIUMINT UNSIGNED" => decode_int::<u32>(row, idx),
        "INT" | "INTEGER" => decode_int::<i32>(row, idx),
        "INT UNSIGNED" | "INTEGER UNSIGNED" => decode_int::<u32>(row, idx),
        "BIGINT" => decode_int::<i64>(row, idx),
        "BIGINT UNSIGNED" => match row.try_get::<u64, _>(idx) {
            Ok(value) => Ok({
                // u64 超 i64::MAX 时用 Text 保值
                if value > i64::MAX as u64 {
                    Value::Text(value.to_string())
                } else {
                    Value::Int(value as i64)
                }
            }),
            Err(error) => fallback_text(row, col, error),
        },

        "FLOAT" => decode_as::<f32, _>(row, col, |value| Value::Float(value as f64)),
        "DOUBLE" => decode_as::<f64, _>(row, col, Value::Float),

        // DECIMAL：BigDecimal 精确解码后转字符串保精度
        "DECIMAL" | "NUMERIC" => {
            decode_as::<BigDecimal, _>(row, col, |value| Value::Text(value.to_string()))
        }

        "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" => {
            decode_as::<String, _>(row, col, Value::Text)
        }

        "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            decode_as::<Vec<u8>, _>(row, col, Value::Bytes)
        }
        // SQLx 将 BIT 视为无符号整数，但线协议值本身是原始大端字节；跳过类型兼容检查保留位模式。
        "BIT" => decode_wire_bytes(row, col),
        // SQLx 未提供 MySQL 空间类型解码；保留包含 SRID/WKB 的原始线协议字节供上层展示或导出。
        "GEOMETRY" | "POINT" | "LINESTRING" | "POLYGON" | "MULTIPOINT" | "MULTILINESTRING"
        | "MULTIPOLYGON" | "GEOMETRYCOLLECTION" => decode_wire_bytes(row, col),

        "DATETIME" => decode_as::<NaiveDateTime, _>(row, col, |value| {
            Value::DateTime(DateTime::<Utc>::from_naive_utc_and_offset(value, Utc))
        }),
        "TIMESTAMP" => decode_as::<DateTime<Utc>, _>(row, col, Value::DateTime),
        "DATE" => decode_as::<NaiveDate, _>(row, col, |value| {
            Value::Text(value.format("%Y-%m-%d").to_string())
        }),
        "TIME" => decode_as::<NaiveTime, _>(row, col, |value| {
            Value::Text(value.format("%H:%M:%S").to_string())
        }),
        "YEAR" => decode_int::<u16>(row, idx),

        "JSON" => decode_as::<serde_json::Value, _>(row, col, Value::Json),

        // ENUM/SET 内部存字符串
        "ENUM" | "SET" => decode_as::<String, _>(row, col, Value::Text),

        _ => fallback_text(row, col, format!("不支持的 MySQL 类型 {type_name}")),
    }
}

fn decode_as<T, F>(row: &MySqlRow, col: &MySqlColumn, convert: F) -> Result<Value>
where
    T: for<'r> sqlx::Decode<'r, sqlx::MySql> + sqlx::Type<sqlx::MySql>,
    F: FnOnce(T) -> Value,
{
    match row.try_get::<T, _>(col.ordinal()) {
        Ok(value) => Ok(convert(value)),
        Err(error) => fallback_text(row, col, error),
    }
}

fn decode_int<T>(row: &MySqlRow, idx: usize) -> Result<Value>
where
    T: for<'r> sqlx::Decode<'r, sqlx::MySql> + sqlx::Type<sqlx::MySql> + Into<i64>,
{
    let col = &row.columns()[idx];
    decode_as::<T, _>(row, col, |value| Value::Int(value.into()))
}

fn decode_wire_bytes(row: &MySqlRow, col: &MySqlColumn) -> Result<Value> {
    row.try_get_unchecked::<Vec<u8>, _>(col.ordinal())
        .map(Value::Bytes)
        .map_err(|error| {
            DomainError::QueryFailed(format!(
                "解码列「{}」({}) 原始字节失败：{error}",
                col.name(),
                col.type_info().name()
            ))
        })
}

/// 类型解码失败后尝试读取原始文本；两种方式都失败则显式中止查询，不能伪装成 NULL。
fn fallback_text(
    row: &MySqlRow,
    col: &MySqlColumn,
    primary_error: impl std::fmt::Display,
) -> Result<Value> {
    row.try_get::<String, _>(col.ordinal())
        .map(Value::Text)
        .map_err(|fallback_error| {
            DomainError::QueryFailed(format!(
                "解码列「{}」({}) 失败：{primary_error}；文本兜底失败：{fallback_error}",
                col.name(),
                col.type_info().name()
            ))
        })
}

/// 将 MySQL 列类型映射为领域列类型。
pub fn map_column_type(data_type: &str, column_type: &str) -> ColumnType {
    let kind = match data_type.to_ascii_uppercase().as_str() {
        "TINYINT" if column_type.eq_ignore_ascii_case("tinyint(1)") => ColumnKind::Bool,
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" | "YEAR" => {
            ColumnKind::Integer
        }
        "DECIMAL" | "NUMERIC" => ColumnKind::Decimal,
        "FLOAT" | "DOUBLE" | "REAL" => ColumnKind::Float,
        "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            ColumnKind::Text
        }
        "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BIT" => {
            ColumnKind::Blob
        }
        "DATE" | "DATETIME" | "TIMESTAMP" | "TIME" => ColumnKind::DateTime,
        "JSON" => ColumnKind::Json,
        _ => ColumnKind::Other,
    };

    ColumnType {
        kind,
        raw_type: column_type.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_int_types() {
        assert_eq!(map_column_type("INT", "int(11)").kind, ColumnKind::Integer);
        assert_eq!(
            map_column_type("BIGINT", "bigint(20)").kind,
            ColumnKind::Integer
        );
        assert_eq!(map_column_type("YEAR", "year").kind, ColumnKind::Integer);
    }

    #[test]
    fn map_tinyint_one_is_bool() {
        assert_eq!(
            map_column_type("TINYINT", "tinyint(1)").kind,
            ColumnKind::Bool
        );
        assert_eq!(
            map_column_type("TINYINT", "tinyint(4)").kind,
            ColumnKind::Integer
        );
    }

    #[test]
    fn map_text_types() {
        assert_eq!(
            map_column_type("VARCHAR", "varchar(255)").kind,
            ColumnKind::Text
        );
        assert_eq!(
            map_column_type("LONGTEXT", "longtext").kind,
            ColumnKind::Text
        );
        assert_eq!(
            map_column_type("ENUM", "enum('a','b')").kind,
            ColumnKind::Text
        );
    }

    #[test]
    fn map_blob_types() {
        assert_eq!(map_column_type("BLOB", "blob").kind, ColumnKind::Blob);
        assert_eq!(map_column_type("BIT", "bit(8)").kind, ColumnKind::Blob);
    }

    #[test]
    fn map_datetime_types() {
        assert_eq!(
            map_column_type("DATETIME", "datetime").kind,
            ColumnKind::DateTime
        );
        assert_eq!(
            map_column_type("TIMESTAMP", "timestamp").kind,
            ColumnKind::DateTime
        );
        assert_eq!(map_column_type("DATE", "date").kind, ColumnKind::DateTime);
    }

    #[test]
    fn map_json() {
        assert_eq!(map_column_type("JSON", "json").kind, ColumnKind::Json);
    }

    #[test]
    fn map_decimal_keeps_precision() {
        let t = map_column_type("DECIMAL", "decimal(10,2)");
        assert_eq!(t.kind, ColumnKind::Decimal);
        assert_eq!(t.raw_type, "decimal(10,2)");
    }

    #[test]
    fn map_unknown() {
        assert_eq!(
            map_column_type("GEOMETRY", "geometry").kind,
            ColumnKind::Other
        );
    }
}
