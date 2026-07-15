//! PG 行解码：PgRow → Domain Value。NUMERIC 用 BigDecimal 转 Text 保精度；array/interval/inet/uuid 等 fallback Text

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use ramag_domain::entities::{ColumnKind, ColumnType, Value};
use ramag_domain::error::{DomainError, Result};
use sqlx::Column as _;
use sqlx::TypeInfo as _;
use sqlx::postgres::{PgColumn, PgRow};
use sqlx::types::BigDecimal;
use sqlx::types::Json as SqlxJson;
use sqlx::{Row, ValueRef};

pub fn decode_row(row: &PgRow) -> Result<Vec<Value>> {
    row.columns()
        .iter()
        .map(|col| decode_column(row, col))
        .collect()
}

fn decode_column(row: &PgRow, col: &PgColumn) -> Result<Value> {
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
        "BOOL" => decode_as::<bool, _>(row, col, Value::Bool),

        "INT2" => decode_as::<i16, _>(row, col, |value| Value::Int(i64::from(value))),
        "INT4" => decode_as::<i32, _>(row, col, |value| Value::Int(i64::from(value))),
        "INT8" => decode_as::<i64, _>(row, col, Value::Int),

        "FLOAT4" => decode_as::<f32, _>(row, col, |value| Value::Float(value as f64)),
        "FLOAT8" => decode_as::<f64, _>(row, col, Value::Float),

        // BigDecimal 保精度
        "NUMERIC" => decode_as::<BigDecimal, _>(row, col, |value| Value::Text(value.to_string())),

        "TEXT" | "VARCHAR" | "CHAR" | "BPCHAR" | "NAME" | "CITEXT" => {
            decode_as::<String, _>(row, col, Value::Text)
        }

        "BYTEA" => decode_as::<Vec<u8>, _>(row, col, Value::Bytes),

        // 带时区
        "TIMESTAMPTZ" => decode_as::<DateTime<Utc>, _>(row, col, Value::DateTime),
        // 无时区按 UTC
        "TIMESTAMP" => decode_as::<NaiveDateTime, _>(row, col, |value| {
            Value::DateTime(DateTime::<Utc>::from_naive_utc_and_offset(value, Utc))
        }),
        "DATE" => decode_as::<NaiveDate, _>(row, col, |value| {
            Value::Text(value.format("%Y-%m-%d").to_string())
        }),
        "TIME" => decode_as::<NaiveTime, _>(row, col, |value| {
            Value::Text(value.format("%H:%M:%S").to_string())
        }),

        "JSON" | "JSONB" => {
            decode_as::<SqlxJson<serde_json::Value>, _>(row, col, |value| Value::Json(value.0))
        }

        "UUID" => decode_as::<uuid::Uuid, _>(row, col, |value| Value::Text(value.to_string())),

        // PG 特有类型（array / range / interval / inet / cidr / macaddr / time tz）走 String 文本兜底
        _ => fallback_text(row, col, format!("不支持的 PostgreSQL 类型 {type_name}")),
    }
}

fn decode_as<T, F>(row: &PgRow, col: &PgColumn, convert: F) -> Result<Value>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    F: FnOnce(T) -> Value,
{
    match row.try_get::<T, _>(col.ordinal()) {
        Ok(value) => Ok(convert(value)),
        Err(error) => fallback_text(row, col, error),
    }
}

/// 类型解码失败后尝试读取原始文本；两种方式都失败则显式中止查询，不能伪装成 NULL。
fn fallback_text(
    row: &PgRow,
    col: &PgColumn,
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

/// 把 information_schema 的 (data_type, full_type) 映射到 ColumnKind
pub fn map_column_kind(data_type: &str, full_type: &str) -> ColumnType {
    let kind = match data_type.to_ascii_lowercase().as_str() {
        "boolean" => ColumnKind::Bool,
        "smallint" | "integer" | "bigint" => ColumnKind::Integer,
        "numeric" | "decimal" => ColumnKind::Decimal,
        "real" | "double precision" => ColumnKind::Float,
        "text" | "character varying" | "character" | "name" => ColumnKind::Text,
        "bytea" => ColumnKind::Blob,
        "date"
        | "timestamp without time zone"
        | "timestamp with time zone"
        | "time without time zone"
        | "time with time zone" => ColumnKind::DateTime,
        "json" | "jsonb" => ColumnKind::Json,
        _ => ColumnKind::Other,
    };
    ColumnType {
        kind,
        raw_type: full_type.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_int_types() {
        assert_eq!(
            map_column_kind("integer", "integer").kind,
            ColumnKind::Integer
        );
        assert_eq!(
            map_column_kind("bigint", "bigint").kind,
            ColumnKind::Integer
        );
        assert_eq!(
            map_column_kind("smallint", "smallint").kind,
            ColumnKind::Integer
        );
    }

    #[test]
    fn map_text_types() {
        assert_eq!(
            map_column_kind("character varying", "character varying(255)").kind,
            ColumnKind::Text
        );
        assert_eq!(map_column_kind("text", "text").kind, ColumnKind::Text);
    }

    #[test]
    fn map_decimal_keeps_precision() {
        let t = map_column_kind("numeric", "numeric(10,2)");
        assert_eq!(t.kind, ColumnKind::Decimal);
        assert_eq!(t.raw_type, "numeric(10,2)");
    }

    #[test]
    fn map_datetime_types() {
        assert_eq!(
            map_column_kind("timestamp with time zone", "timestamptz").kind,
            ColumnKind::DateTime
        );
        assert_eq!(map_column_kind("date", "date").kind, ColumnKind::DateTime);
    }

    #[test]
    fn map_jsonb() {
        assert_eq!(map_column_kind("jsonb", "jsonb").kind, ColumnKind::Json);
    }

    #[test]
    fn map_unknown_falls_to_other() {
        assert_eq!(
            map_column_kind("interval", "interval").kind,
            ColumnKind::Other
        );
        assert_eq!(map_column_kind("uuid", "uuid").kind, ColumnKind::Other);
    }
}
