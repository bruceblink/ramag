//! PG 行解码：PgRow → Domain Value。常见原生类型转可读文本，未知二进制类型保留为 Bytes

use std::net::{Ipv4Addr, Ipv6Addr};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use ramag_domain::entities::{ColumnKind, ColumnType, Value};
use ramag_domain::error::{DomainError, Result};
use sqlx::Column as _;
use sqlx::TypeInfo as _;
use sqlx::postgres::types::{PgInterval, PgRange, PgTimeTz};
use sqlx::postgres::{PgColumn, PgRow, PgTypeKind, PgValueFormat, PgValueRef};
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

    let raw = raw_value(row, col)?;
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
        "TIMETZ" => decode_as::<PgTimeTz<NaiveTime, FixedOffset>, _>(row, col, |value| {
            Value::Text(format!(
                "{}{}",
                value.time.format("%H:%M:%S%.f"),
                value.offset
            ))
        }),
        "INTERVAL" => decode_as::<PgInterval, _>(row, col, |value| {
            Value::Text(format!(
                "{} mons {} days {} microseconds",
                value.months, value.days, value.microseconds
            ))
        }),

        "JSON" | "JSONB" => {
            decode_as::<SqlxJson<serde_json::Value>, _>(row, col, |value| Value::Json(value.0))
        }

        "UUID" => decode_as::<uuid::Uuid, _>(row, col, |value| Value::Text(value.to_string())),

        "INT4[]" => decode_int_array(row, col),
        "TEXT[]" => decode_text_array(row, col),
        "INT4RANGE" => {
            decode_as::<PgRange<i32>, _>(row, col, |value| Value::Text(value.to_string()))
        }
        "INET" | "CIDR" => decode_network(row, col),
        "MACADDR" | "MACADDR8" => decode_mac_address(row, col),
        "BIT" | "VARBIT" => decode_bit_string(row, col),
        name if name.eq_ignore_ascii_case("xml") => decode_raw_utf8(row, col),

        // 自定义 enum 的二进制表示就是标签文本；其它未知类型保留原始字节，避免整条查询失败。
        _ => decode_unknown(row, col),
    }
}

fn raw_value<'r>(row: &'r PgRow, col: &PgColumn) -> Result<PgValueRef<'r>> {
    row.try_get_raw(col.ordinal()).map_err(|error| {
        DomainError::QueryFailed(format!(
            "读取列「{}」({}) 原始值失败：{error}",
            col.name(),
            col.type_info().name()
        ))
    })
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

fn decode_int_array(row: &PgRow, col: &PgColumn) -> Result<Value> {
    match row.try_get::<Vec<Option<i32>>, _>(col.ordinal()) {
        Ok(value) => serde_json::to_string(&value)
            .map(Value::Text)
            .map_err(|error| {
                DomainError::QueryFailed(format!(
                    "序列化列「{}」(INT4[]) 失败：{error}",
                    col.name()
                ))
            }),
        Err(error) => fallback_text(row, col, error),
    }
}

fn decode_text_array(row: &PgRow, col: &PgColumn) -> Result<Value> {
    match row.try_get::<Vec<Option<String>>, _>(col.ordinal()) {
        Ok(value) => serde_json::to_string(&value)
            .map(Value::Text)
            .map_err(|error| {
                DomainError::QueryFailed(format!(
                    "序列化列「{}」(TEXT[]) 失败：{error}",
                    col.name()
                ))
            }),
        Err(error) => fallback_text(row, col, error),
    }
}

fn decode_network(row: &PgRow, col: &PgColumn) -> Result<Value> {
    let raw = raw_value(row, col)?;
    if raw.format() == PgValueFormat::Text {
        return raw
            .as_str()
            .map(|value| Value::Text(value.to_string()))
            .map_err(|error| decode_raw_error(col, error));
    }

    let bytes = raw
        .as_bytes()
        .map_err(|error| decode_raw_error(col, error))?;
    if bytes.len() < 4 {
        return Err(decode_data_error(col, "网络值长度不足"));
    }
    let prefix = bytes[1];
    let address_len = usize::from(bytes[3]);
    if bytes.len() != 4 + address_len {
        return Err(decode_data_error(col, "网络值地址长度不匹配"));
    }

    let (address, full_prefix) = match (bytes[0], address_len) {
        (2, 4) => (
            Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]).to_string(),
            32,
        ),
        (3, 16) => {
            let octets: [u8; 16] = bytes[4..]
                .try_into()
                .map_err(|_| decode_data_error(col, "IPv6 地址长度不正确"))?;
            (Ipv6Addr::from(octets).to_string(), 128)
        }
        _ => return Err(decode_data_error(col, "未知网络地址族或长度")),
    };

    if col.type_info().name() == "CIDR" || prefix != full_prefix {
        Ok(Value::Text(format!("{address}/{prefix}")))
    } else {
        Ok(Value::Text(address))
    }
}

fn decode_mac_address(row: &PgRow, col: &PgColumn) -> Result<Value> {
    let raw = raw_value(row, col)?;
    if raw.format() == PgValueFormat::Text {
        return raw
            .as_str()
            .map(|value| Value::Text(value.to_string()))
            .map_err(|error| decode_raw_error(col, error));
    }

    let bytes = raw
        .as_bytes()
        .map_err(|error| decode_raw_error(col, error))?;
    if !matches!(bytes.len(), 6 | 8) {
        return Err(decode_data_error(col, "MAC 地址长度应为 6 或 8 bytes"));
    }
    Ok(Value::Text(
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    ))
}

fn decode_bit_string(row: &PgRow, col: &PgColumn) -> Result<Value> {
    let raw = raw_value(row, col)?;
    if raw.format() == PgValueFormat::Text {
        return raw
            .as_str()
            .map(|value| Value::Text(value.to_string()))
            .map_err(|error| decode_raw_error(col, error));
    }

    let bytes = raw
        .as_bytes()
        .map_err(|error| decode_raw_error(col, error))?;
    let length_bytes: [u8; 4] = bytes
        .get(..4)
        .ok_or_else(|| decode_data_error(col, "位串长度字段缺失"))?
        .try_into()
        .map_err(|_| decode_data_error(col, "位串长度字段不正确"))?;
    let bit_len = usize::try_from(u32::from_be_bytes(length_bytes))
        .map_err(|_| decode_data_error(col, "位串长度超出平台范围"))?;
    let value_bytes = bit_len.div_ceil(8);
    if bytes.len() != 4 + value_bytes {
        return Err(decode_data_error(col, "位串数据长度不匹配"));
    }

    let mut text = String::with_capacity(bit_len);
    for index in 0..bit_len {
        let byte = bytes[4 + index / 8];
        let mask = 1_u8 << (7 - index % 8);
        text.push(if byte & mask == 0 { '0' } else { '1' });
    }
    Ok(Value::Text(text))
}

fn decode_raw_utf8(row: &PgRow, col: &PgColumn) -> Result<Value> {
    raw_value(row, col)?
        .as_str()
        .map(|value| Value::Text(value.to_string()))
        .map_err(|error| decode_raw_error(col, error))
}

fn decode_unknown(row: &PgRow, col: &PgColumn) -> Result<Value> {
    let raw = raw_value(row, col)?;
    if raw.format() == PgValueFormat::Text || matches!(col.type_info().kind(), PgTypeKind::Enum(_))
    {
        return raw
            .as_str()
            .map(|value| Value::Text(value.to_string()))
            .map_err(|error| decode_raw_error(col, error));
    }
    raw.as_bytes()
        .map(|value| Value::Bytes(value.to_vec()))
        .map_err(|error| decode_raw_error(col, error))
}

fn decode_raw_error(col: &PgColumn, error: impl std::fmt::Display) -> DomainError {
    DomainError::QueryFailed(format!(
        "读取列「{}」({}) 原始值失败：{error}",
        col.name(),
        col.type_info().name()
    ))
}

fn decode_data_error(col: &PgColumn, detail: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解码列「{}」({}) 失败：{detail}",
        col.name(),
        col.type_info().name()
    ))
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
