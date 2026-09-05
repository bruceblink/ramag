//! SQL 查询与结果集。
mod formatting;

pub use formatting::json_pretty_bounded;
use formatting::*;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::contains_case_insensitive;

pub const MAX_SQL_QUERY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub sql: String,
    /// 将多条语句作为一个事务执行；仅用于后端确实支持事务化 DDL 的场景。
    #[serde(default)]
    pub transactional: bool,
    /// 会话默认库，driver 执行前发 USE 切换
    #[serde(default)]
    pub default_schema: Option<String>,
    /// 驱动可为最外层 SELECT/WITH 注入的 LIMIT；当前 UI 不使用。
    #[serde(default)]
    pub auto_limit: Option<u32>,
    /// 结果常驻内存上限；`None` 使用交互结果上限。
    #[serde(default)]
    pub result_byte_limit: Option<usize>,
}

impl Query {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            transactional: false,
            default_schema: None,
            auto_limit: None,
            result_byte_limit: None,
        }
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.default_schema = Some(schema.into());
        self
    }

    pub fn transactional(mut self) -> Self {
        self.transactional = true;
        self
    }

    pub fn with_auto_limit(mut self, limit: Option<u32>) -> Self {
        self.auto_limit = limit;
        self
    }

    pub fn with_result_byte_limit(mut self, limit: usize) -> Self {
        self.result_byte_limit = Some(limit);
        self
    }

    pub fn validate(&self) -> crate::error::Result<()> {
        if self.sql.len() > MAX_SQL_QUERY_BYTES {
            return Err(crate::error::DomainError::InvalidConfig(format!(
                "SQL 内容超过 {} MiB 安全上限",
                MAX_SQL_QUERY_BYTES / 1024 / 1024
            )));
        }
        if self.sql.contains('\0') {
            return Err(crate::error::DomainError::InvalidConfig(
                "SQL 内容不能包含 NUL 字符".into(),
            ));
        }
        if let Some(limit) = self.result_byte_limit
            && (limit == 0 || limit > super::MAX_INTERACTIVE_RESULT_BYTES)
        {
            return Err(crate::error::DomainError::InvalidConfig(format!(
                "SQL 结果字节预算必须在 1 字节到 {} MiB 之间",
                super::MAX_INTERACTIVE_RESULT_BYTES / 1024 / 1024
            )));
        }
        if let Some(schema) = &self.default_schema
            && (schema.len() > super::connection::MAX_CONNECTION_IDENTIFIER_BYTES
                || schema.chars().any(char::is_control))
        {
            return Err(crate::error::DomainError::InvalidConfig(format!(
                "默认 schema 超过 {} KiB 上限或包含控制字符",
                super::connection::MAX_CONNECTION_IDENTIFIER_BYTES / 1024
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    /// 列类型名，与 columns 一一对应；driver 不提供时为空。仅 UI 表头展示
    #[serde(default)]
    pub column_types: Vec<String>,
    pub rows: Vec<Row>,
    pub affected_rows: u64,
    pub elapsed_ms: u64,
    /// MySQL SHOW WARNINGS；多语句执行时累积所有 statement 的警告
    #[serde(default)]
    pub warnings: Vec<Warning>,
    /// 是否因字节预算截断。
    #[serde(default)]
    pub truncated: bool,
}

impl QueryResult {
    pub fn retained_bytes(&self) -> u64 {
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(
                self.columns
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
            )
            .saturating_add(
                self.column_types
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
            )
            .saturating_add(
                self.rows
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Row>()),
            )
            .saturating_add(
                self.warnings
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Warning>()),
            );
        for column in &self.columns {
            bytes = bytes.saturating_add(column.capacity());
        }
        for column_type in &self.column_types {
            bytes = bytes.saturating_add(column_type.capacity());
        }
        for row in &self.rows {
            // Row 容量已计入，只累加动态内容。
            bytes = bytes.saturating_add(
                usize::try_from(row.retained_bytes())
                    .unwrap_or(usize::MAX)
                    .saturating_sub(std::mem::size_of::<Row>()),
            );
        }
        for warning in &self.warnings {
            bytes = bytes
                .saturating_add(warning.level.capacity())
                .saturating_add(warning.message.capacity());
        }
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    pub level: String,
    pub code: u32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<Value>,
}

impl Row {
    /// 结果集常驻内存的保守估算；用于流式查询的客户端总预算。
    pub fn retained_bytes(&self) -> u64 {
        let mut bytes = std::mem::size_of::<Row>().saturating_add(
            self.values
                .capacity()
                .saturating_mul(std::mem::size_of::<Value>()),
        );
        for value in &self.values {
            bytes = bytes.saturating_add(value_dynamic_bytes(value));
        }
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    DateTime(chrono::DateTime<chrono::Utc>),
    /// MySQL JSON 列、PG jsonb
    Json(serde_json::Value),
}

fn value_dynamic_bytes(value: &Value) -> usize {
    match value {
        Value::Text(text) => text.capacity(),
        Value::Bytes(bytes) => bytes.capacity(),
        Value::Json(value) => json_dynamic_bytes(value),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::DateTime(_) => 0,
    }
}

fn json_dynamic_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(text) => text.capacity(),
        serde_json::Value::Array(values) => {
            let mut bytes = values
                .capacity()
                .saturating_mul(std::mem::size_of::<serde_json::Value>());
            for value in values {
                bytes = bytes.saturating_add(json_dynamic_bytes(value));
            }
            bytes
        }
        serde_json::Value::Object(values) => {
            // Map 的节点实现细节不公开；按键值结构再加三个指针的节点开销保守估算。
            let entry_bytes = std::mem::size_of::<(String, serde_json::Value)>()
                .saturating_add(3 * std::mem::size_of::<usize>());
            let mut bytes = values.len().saturating_mul(entry_bytes);
            for (key, value) in values {
                bytes = bytes
                    .saturating_add(key.capacity())
                    .saturating_add(json_dynamic_bytes(value));
            }
            bytes
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => 0,
    }
}

impl Value {
    pub fn display_preview(&self, max_len: usize) -> String {
        match self {
            Value::Null => "NULL".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Text(s) => sanitize_inline(&truncate(s, max_len)),
            Value::Bytes(b) => {
                uuid_preview(b, max_len).unwrap_or_else(|| format!("[{} bytes]", b.len()))
            }
            Value::DateTime(dt) => dt.to_rfc3339(),
            Value::Json(v) => {
                let byte_limit = max_len.saturating_mul(4).saturating_add(1);
                let (text, _) = serialize_json_prefix(v, false, byte_limit);
                truncate(&text, max_len)
            }
        }
    }

    /// 行过滤用大小写不敏感匹配。常见 ASCII 文本不分配完整副本；其它类型保持展示语义。
    pub fn contains_query_lower(&self, query_lower: &str) -> bool {
        match self {
            Value::Null => contains_case_insensitive("NULL", query_lower),
            Value::Bool(value) => {
                contains_case_insensitive(if *value { "true" } else { "false" }, query_lower)
            }
            Value::Int(value) => value.to_string().contains(query_lower),
            Value::Float(value) => value.to_string().contains(query_lower),
            Value::Text(value) => contains_case_insensitive(value, query_lower),
            Value::Bytes(value) => {
                contains_case_insensitive(&format!("[{} bytes]", value.len()), query_lower)
            }
            Value::DateTime(value) => contains_case_insensitive(&value.to_rfc3339(), query_lower),
            Value::Json(value) => json_contains_query(value, query_lower),
        }
    }

    /// 转 SQL 字面量（MySQL 方言，向后兼容）。新代码优先用 [`Value::to_sql_literal_for`]
    pub fn to_sql_literal(&self) -> String {
        self.to_sql_literal_for(super::connection::DriverKind::Mysql)
    }

    /// 转 SQL 字面量，按 driver 方言处理字符串转义 / 字节 / 时间。
    /// - 字符串转义：MySQL 双写反斜杠；PG/SQLite 按标准 SQL 只双写单引号
    /// - Bytes：MySQL `0xHEX` / PG `'\xHEX'` / SQLite `X'HEX'`
    /// - DateTime：亚秒精度保留（`%.6f`），避免高精度时间戳无法命中
    pub fn to_sql_literal_for(&self, driver: super::connection::DriverKind) -> String {
        use super::connection::DriverKind;
        let is_pg = matches!(driver, DriverKind::Postgres);
        let is_mysql = matches!(driver, DriverKind::Mysql);
        match self {
            Value::Null => "NULL".to_string(),
            Value::Bool(b) => {
                if *b {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                }
            }
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Text(s) => format!("'{}'", escape_sql_string(s, is_mysql)),
            Value::Bytes(b) => {
                let hex = encode_hex(b);
                if is_pg {
                    // PG bytea 输入字面量：'\xDEADBEEF'
                    format!("'\\x{hex}'")
                } else if driver == DriverKind::Sqlite {
                    format!("X'{hex}'")
                } else {
                    format!("0x{hex}")
                }
            }
            Value::DateTime(dt) => {
                // 保留亚秒（去尾零由 %.f 语义处理），命中高精度时间列
                format!("'{}'", dt.format("%Y-%m-%d %H:%M:%S%.6f"))
            }
            Value::Json(v) => format!("'{}'", escape_sql_string(&v.to_string(), is_mysql)),
        }
    }

    /// 在不构造完整 SQL 字面量的前提下计算长度；超过 `max_bytes` 返回 `None`。
    pub fn bounded_sql_literal_len_for(
        &self,
        driver: super::connection::DriverKind,
        max_bytes: usize,
    ) -> Option<usize> {
        use super::connection::DriverKind;
        let is_pg = matches!(driver, DriverKind::Postgres);
        let is_mysql = matches!(driver, DriverKind::Mysql);
        let length = match self {
            Value::Null => 4,
            Value::Bool(true) => 4,
            Value::Bool(false) => 5,
            Value::Int(value) => value.to_string().len(),
            Value::Float(value) => value.to_string().len(),
            Value::Text(value) => escaped_sql_literal_len(value.as_bytes(), is_mysql, max_bytes)?,
            Value::Bytes(value) => value.len().checked_mul(2)?.checked_add(
                if is_pg || driver == DriverKind::Sqlite {
                    4
                } else {
                    2
                },
            )?,
            Value::DateTime(value) => value.format("%Y-%m-%d %H:%M:%S%.6f").to_string().len() + 2,
            Value::Json(value) => json_sql_literal_len(value, is_mysql, max_bytes)?,
        };
        (length <= max_bytes).then_some(length)
    }

    pub fn display_for_edit(&self) -> String {
        match self {
            Value::Json(v) => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
            other => other.to_clipboard_string(),
        }
    }

    /// 编辑弹框初值的有界版本；第二个返回值表示内容已截断，只能只读展示。
    pub fn display_for_edit_bounded(&self, max_bytes: usize) -> (String, bool) {
        match self {
            Value::Text(text) => bounded_text(text, max_bytes),
            Value::Bytes(bytes) => bounded_hex(bytes, max_bytes),
            Value::Json(value) => {
                let (text, truncated) = serialize_json_prefix(value, true, max_bytes);
                if truncated {
                    (with_truncation_notice(text, max_bytes), true)
                } else {
                    (text, false)
                }
            }
            other => bounded_text(&other.to_clipboard_string(), max_bytes),
        }
    }

    pub fn to_clipboard_string(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Text(s) => s.clone(),
            Value::Bytes(b) => encode_hex(b),
            Value::DateTime(dt) => dt.to_rfc3339(),
            Value::Json(v) => v.to_string(),
        }
    }
}

/// Formats a 16-byte value with the same byte order as MySQL `BIN_TO_UUID(value, 0)`.
/// Values with another length remain compact binary previews.
fn uuid_preview(bytes: &[u8], max_len: usize) -> Option<String> {
    if bytes.len() != 16 {
        return None;
    }
    let raw: [u8; 16] = bytes.try_into().ok()?;
    Some(truncate(&Uuid::from_bytes(raw).to_string(), max_len))
}

#[cfg(test)]
mod tests;
