//! 查询与结果集实体

use serde::{Deserialize, Serialize};

use super::contains_case_insensitive;

pub const MAX_SQL_QUERY_BYTES: usize = 32 * 1024 * 1024;

/// 一次 SQL 查询请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub sql: String,
    /// 会话默认库，driver 执行前发 USE 切换
    #[serde(default)]
    pub default_schema: Option<String>,
    /// driver 层可选的自动 LIMIT 注入能力：Some(n) 给未带 LIMIT 的最外层 SELECT/WITH
    /// 追加 `LIMIT n`。当前 UI 不再使用（恒传 None），保留供 driver 兜底与未来复用
    #[serde(default)]
    pub auto_limit: Option<u32>,
    /// 调用方的结果常驻内存预算；None 使用交互查询的 256 MiB 硬上限。
    #[serde(default)]
    pub result_byte_limit: Option<usize>,
}

impl Query {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            default_schema: None,
            auto_limit: None,
            result_byte_limit: None,
        }
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.default_schema = Some(schema.into());
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

/// 查询结果（INSERT/UPDATE 也走这个，rows 空、affected_rows 有值）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    /// 列类型名，与 columns 一一对应；driver 不提供时为空。仅 UI 表头展示
    #[serde(default)]
    pub column_types: Vec<String>,
    pub rows: Vec<Row>,
    /// INSERT/UPDATE/DELETE 受影响行数
    pub affected_rows: u64,
    pub elapsed_ms: u64,
    /// MySQL SHOW WARNINGS；多语句执行时累积所有 statement 的警告
    #[serde(default)]
    pub warnings: Vec<Warning>,
    /// 驱动因结果字节预算停止读取，调用方可据此继续分页。
    #[serde(default)]
    pub truncated: bool,
}

impl QueryResult {
    /// 结果集在客户端的常驻内存保守估算，供跨标签总预算使用。
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
            // Row 本体已计入 rows 的容量，只追加其动态内容。
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

/// 服务端警告（MySQL SHOW WARNINGS 一行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    /// "Note" / "Warning" / "Error"
    pub level: String,
    /// 对应 mysql_errno()
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

/// 单元格值。UI 按 variant 选渲染方式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// UTC 纳秒精度
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
    /// UI 显示用的短预览（截断长字符串）
    pub fn display_preview(&self, max_len: usize) -> String {
        match self {
            Value::Null => "NULL".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Text(s) => sanitize_inline(&truncate(s, max_len)),
            Value::Bytes(b) => format!("[{} bytes]", b.len()),
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
    /// - 字符串转义：MySQL 双写反斜杠；PG 默认 standard_conforming_strings，反斜杠是字面量不双写
    /// - Bytes：MySQL `0xHEX` / PG `'\xHEX'`（bytea 输入）
    /// - DateTime：亚秒精度保留（`%.6f`），避免高精度时间戳无法命中
    pub fn to_sql_literal_for(&self, driver: super::connection::DriverKind) -> String {
        use super::connection::DriverKind;
        let is_pg = matches!(driver, DriverKind::Postgres);
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
            Value::Text(s) => format!("'{}'", escape_sql_string(s, is_pg)),
            Value::Bytes(b) => {
                let hex = encode_hex(b);
                if is_pg {
                    // PG bytea 输入字面量：'\xDEADBEEF'
                    format!("'\\x{hex}'")
                } else {
                    format!("0x{hex}")
                }
            }
            Value::DateTime(dt) => {
                // 保留亚秒（去尾零由 %.f 语义处理），命中高精度时间列
                format!("'{}'", dt.format("%Y-%m-%d %H:%M:%S%.6f"))
            }
            Value::Json(v) => format!("'{}'", escape_sql_string(&v.to_string(), is_pg)),
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
        let length = match self {
            Value::Null => 4,
            Value::Bool(true) => 4,
            Value::Bool(false) => 5,
            Value::Int(value) => value.to_string().len(),
            Value::Float(value) => value.to_string().len(),
            Value::Text(value) => escaped_sql_literal_len(value.as_bytes(), is_pg, max_bytes)?,
            Value::Bytes(value) => {
                value
                    .len()
                    .checked_mul(2)?
                    .checked_add(if is_pg { 4 } else { 2 })?
            }
            Value::DateTime(value) => value.format("%Y-%m-%d %H:%M:%S%.6f").to_string().len() + 2,
            Value::Json(value) => json_sql_literal_len(value, is_pg, max_bytes)?,
        };
        (length <= max_bytes).then_some(length)
    }

    /// 单元格编辑初值：JSON 走 pretty 多行，其余等价 clipboard 形式
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

    /// 剪贴板字符串（完整，不截断）。Null→空串、Bytes→hex、DateTime→RFC3339、Json→紧凑
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

fn escaped_sql_literal_len(bytes: &[u8], is_pg: bool, max_bytes: usize) -> Option<usize> {
    let mut length = 2usize;
    for byte in bytes {
        let added = usize::from(*byte == b'\'' || (!is_pg && *byte == b'\\')) + 1;
        length = length.checked_add(added)?;
        if length > max_bytes {
            return None;
        }
    }
    Some(length)
}

fn json_sql_literal_len(value: &serde_json::Value, is_pg: bool, max_bytes: usize) -> Option<usize> {
    let mut writer = SqlLiteralLengthWriter {
        length: 2,
        limit: max_bytes,
        is_pg,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut writer, value);
    (result.is_ok() && !writer.exceeded).then_some(writer.length)
}

struct SqlLiteralLengthWriter {
    length: usize,
    limit: usize,
    is_pg: bool,
    exceeded: bool,
}

impl std::io::Write for SqlLiteralLengthWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        for byte in buffer {
            let added = usize::from(*byte == b'\'' || (!self.is_pg && *byte == b'\\')) + 1;
            let Some(next) = self.length.checked_add(added) else {
                self.exceeded = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "SQL literal length overflow",
                ));
            };
            if next > self.limit {
                self.exceeded = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "SQL literal length limit reached",
                ));
            }
            self.length = next;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn json_contains_query(value: &serde_json::Value, query_lower: &str) -> bool {
    match value {
        serde_json::Value::Null => contains_case_insensitive("null", query_lower),
        serde_json::Value::Bool(value) => {
            contains_case_insensitive(if *value { "true" } else { "false" }, query_lower)
        }
        serde_json::Value::Number(value) => value.to_string().contains(query_lower),
        serde_json::Value::String(value) => contains_case_insensitive(value, query_lower),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_contains_query(value, query_lower)),
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            contains_case_insensitive(key, query_lower) || json_contains_query(value, query_lower)
        }),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        output.push(char::from(DIGITS[(byte >> 4) as usize]));
        output.push(char::from(DIGITS[(byte & 0x0f) as usize]));
    }
    output
}

fn bounded_hex(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    let full_len = bytes.len().saturating_mul(2);
    if full_len <= max_bytes {
        return (encode_hex(bytes), false);
    }
    let mut output = String::with_capacity(max_bytes);
    let take = bytes.len().min(max_bytes / 2);
    for &byte in &bytes[..take] {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        output.push(char::from(DIGITS[(byte >> 4) as usize]));
        output.push(char::from(DIGITS[(byte & 0x0f) as usize]));
    }
    (with_truncation_notice(output, max_bytes), true)
}

fn bounded_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let end = floor_char_boundary(text, max_bytes);
    (
        with_truncation_notice(text[..end].to_string(), max_bytes),
        true,
    )
}

fn with_truncation_notice(mut text: String, max_bytes: usize) -> String {
    const NOTICE: &str = "\n\n[内容过大，仅显示开头部分]";
    if max_bytes <= NOTICE.len() {
        let end = floor_char_boundary(&text, max_bytes);
        text.truncate(end);
        return text;
    }
    let content_limit = max_bytes - NOTICE.len();
    if text.len() > content_limit {
        let end = floor_char_boundary(&text, content_limit);
        text.truncate(end);
    }
    text.push_str(NOTICE);
    text
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn serialize_json_prefix(
    value: &serde_json::Value,
    pretty: bool,
    max_bytes: usize,
) -> (String, bool) {
    let mut writer = BoundedJsonWriter::new(max_bytes);
    let result = if pretty {
        serde_json::to_writer_pretty(&mut writer, value)
    } else {
        serde_json::to_writer(&mut writer, value)
    };
    if result.is_err() && !writer.truncated {
        return ("[JSON 序列化失败]".to_string(), false);
    }
    let truncated = writer.truncated;
    let valid_len = std::str::from_utf8(&writer.bytes)
        .map(|text| text.len())
        .unwrap_or_else(|error| error.valid_up_to());
    writer.bytes.truncate(valid_len);
    let text = String::from_utf8(writer.bytes).unwrap_or_default();
    (text, truncated)
}

/// 完整 pretty JSON 仅在不超过字节上限时返回，序列化过程本身也不会越界分配。
pub fn json_pretty_bounded(value: &serde_json::Value, max_bytes: usize) -> Option<String> {
    let mut writer = BoundedJsonWriter::new(max_bytes);
    if serde_json::to_writer_pretty(&mut writer, value).is_err() || writer.truncated {
        return None;
    }
    String::from_utf8(writer.bytes).ok()
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(64 * 1024)),
            limit,
            truncated: false,
        }
    }
}

impl std::io::Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if buffer.len() <= remaining {
            self.bytes.extend_from_slice(buffer);
            return Ok(buffer.len());
        }
        self.bytes.extend_from_slice(&buffer[..remaining]);
        self.truncated = true;
        Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "JSON preview limit reached",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// SQL 字符串字面量转义。单引号一律双写（两方言通用）；
/// 反斜杠仅 MySQL 双写——PG 默认 standard_conforming_strings=on 时反斜杠是字面量，
/// 双写会把 `a\b` 污染成 `a\\b`（写歪数据 / WHERE 匹配不中）
fn escape_sql_string(s: &str, is_pg: bool) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '\\' if !is_pg => out.push_str("\\\\"),
            '\'' => out.push_str("''"),
            _ => out.push(ch),
        }
    }
    out
}

fn truncate(s: &str, max_len: usize) -> String {
    let mut chars = s.chars();
    let mut prefix: String = chars.by_ref().take(max_len).collect();
    if chars.next().is_some() {
        prefix.push('…');
    }
    prefix
}

/// 单行预览清洗：换行符（\n / \r）替换为空格。
/// GPUI 单行文本 shaping 断言不允许 \n（含 \n 直接 panic→abort）；仅用于显示预览，
/// 不影响 to_clipboard_string / display_for_edit 等完整取值。无换行时零拷贝
fn sanitize_inline(s: &str) -> String {
    if s.contains(['\n', '\r']) {
        s.replace(['\n', '\r'], " ")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn clipboard_null_is_empty() {
        assert_eq!(Value::Null.to_clipboard_string(), "");
    }

    #[test]
    fn clipboard_primitive() {
        assert_eq!(Value::Bool(true).to_clipboard_string(), "true");
        assert_eq!(Value::Int(-42).to_clipboard_string(), "-42");
        assert_eq!(Value::Float(2.5).to_clipboard_string(), "2.5");
    }

    #[test]
    fn query_match_is_case_insensitive_for_ascii_and_unicode() {
        assert!(Value::Text("Hello Rust".into()).contains_query_lower("hello"));
        assert!(Value::Text("你好世界".into()).contains_query_lower("世界"));
        assert!(!Value::Text("Hello".into()).contains_query_lower("world"));
        let json = Value::Json(serde_json::json!({"UserName": ["Alice", "你好世界"]}));
        assert!(json.contains_query_lower("username"));
        assert!(json.contains_query_lower("alice"));
        assert!(json.contains_query_lower("世界"));
    }

    #[test]
    fn preview_truncates_without_splitting_unicode() {
        assert_eq!(Value::Text("你好世界".into()).display_preview(2), "你好…");
        assert_eq!(Value::Text("你好".into()).display_preview(2), "你好");
    }

    #[test]
    fn pretty_json_limit_is_enforced_during_serialization()
    -> std::result::Result<(), serde_json::Error> {
        let value = serde_json::json!({"name": "alice", "items": [1, 2]});
        let expected = serde_json::to_string_pretty(&value)?;

        assert_eq!(
            json_pretty_bounded(&value, expected.len()),
            Some(expected.clone())
        );
        assert!(json_pretty_bounded(&value, expected.len() - 1).is_none());
        Ok(())
    }

    #[test]
    fn clipboard_text_not_truncated() {
        let long: String = "字".repeat(200);
        assert_eq!(Value::Text(long.clone()).to_clipboard_string(), long);
    }

    #[test]
    fn clipboard_bytes_hex() {
        let v = Value::Bytes(vec![0x00, 0xAB, 0xff]);
        assert_eq!(v.to_clipboard_string(), "00abff");
    }

    #[test]
    fn clipboard_datetime_rfc3339() {
        let dt = chrono::Utc
            .with_ymd_and_hms(2026, 4, 26, 17, 30, 0)
            .unwrap();
        let s = Value::DateTime(dt).to_clipboard_string();
        assert!(s.starts_with("2026-04-26T17:30:00"));
    }

    #[test]
    fn sql_literal_basic() {
        assert_eq!(Value::Null.to_sql_literal(), "NULL");
        assert_eq!(Value::Bool(true).to_sql_literal(), "TRUE");
        assert_eq!(Value::Bool(false).to_sql_literal(), "FALSE");
        assert_eq!(Value::Int(42).to_sql_literal(), "42");
    }

    #[test]
    fn sql_literal_text_escapes_quote() {
        assert_eq!(
            Value::Text("O'Reilly".to_string()).to_sql_literal(),
            "'O''Reilly'"
        );
        assert_eq!(Value::Text("a\\b".to_string()).to_sql_literal(), "'a\\\\b'");
    }

    #[test]
    fn sql_literal_bytes_hex() {
        assert_eq!(
            Value::Bytes(vec![0x00, 0xab, 0xff]).to_sql_literal(),
            "0x00abff"
        );
    }

    #[test]
    fn sql_literal_datetime_mysql_format() {
        let dt = chrono::Utc
            .with_ymd_and_hms(2026, 4, 8, 17, 31, 15)
            .unwrap();
        // 带亚秒精度（整秒时为 .000000）：高精度时间列才能命中
        assert_eq!(
            Value::DateTime(dt).to_sql_literal(),
            "'2026-04-08 17:31:15.000000'"
        );
    }

    #[test]
    fn sql_literal_pg_dialect() {
        use super::super::connection::DriverKind;
        // PG：反斜杠不双写（standard_conforming_strings）
        assert_eq!(
            Value::Text("a\\b".to_string()).to_sql_literal_for(DriverKind::Postgres),
            "'a\\b'"
        );
        // PG bytea：'\xHEX'
        assert_eq!(
            Value::Bytes(vec![0xde, 0xad]).to_sql_literal_for(DriverKind::Postgres),
            "'\\xdead'"
        );
        // 单引号两方言都双写
        assert_eq!(
            Value::Text("O'x".to_string()).to_sql_literal_for(DriverKind::Postgres),
            "'O''x'"
        );
    }

    #[test]
    fn sql_literal_length_is_checked_without_building_large_output() {
        use super::super::connection::DriverKind;

        let text = Value::Text("a'\\b".into());
        let mysql = text.to_sql_literal_for(DriverKind::Mysql);
        assert_eq!(
            text.bounded_sql_literal_len_for(DriverKind::Mysql, mysql.len()),
            Some(mysql.len())
        );
        assert!(
            text.bounded_sql_literal_len_for(DriverKind::Mysql, mysql.len() - 1)
                .is_none()
        );

        let json = Value::Json(serde_json::json!({"text": "O'Reilly\\path"}));
        let postgres = json.to_sql_literal_for(DriverKind::Postgres);
        assert_eq!(
            json.bounded_sql_literal_len_for(DriverKind::Postgres, postgres.len()),
            Some(postgres.len())
        );
    }

    #[test]
    fn preview_text_strips_newlines() {
        // 含换行的文本预览必须压成单行，否则结果表格渲染 panic
        let v = Value::Text("line1\nline2\r\nline3".to_string());
        let p = v.display_preview(80);
        assert!(!p.contains('\n') && !p.contains('\r'));
    }

    #[test]
    fn clipboard_json_minified() {
        let v = Value::Json(serde_json::json!({"a": 1, "b": [2, 3]}));
        let s = v.to_clipboard_string();
        assert!(!s.contains("\n"));
        assert!(s.contains("\"a\":1"));
    }

    #[test]
    fn row_retained_bytes_tracks_dynamic_payloads() {
        let small = Row {
            values: vec![Value::Text("a".into())],
        };
        let large = Row {
            values: vec![
                Value::Text("x".repeat(1024)),
                Value::Bytes(vec![0; 2048]),
                Value::Json(serde_json::json!({"items": ["y".repeat(512)]})),
            ],
        };

        assert!(large.retained_bytes() > small.retained_bytes() + 3_000);
    }

    #[test]
    fn sql_query_and_schema_have_explicit_boundaries() {
        let valid = Query::new("x".repeat(MAX_SQL_QUERY_BYTES));
        assert!(valid.validate().is_ok());
        assert!(
            Query::new("x".repeat(MAX_SQL_QUERY_BYTES + 1))
                .validate()
                .is_err()
        );
        assert!(Query::new("select\0 1").validate().is_err());
        assert!(
            Query::new("select 1")
                .with_result_byte_limit(super::super::TRANSFER_BATCH_BYTES)
                .validate()
                .is_ok()
        );
        assert!(
            Query::new("select 1")
                .with_result_byte_limit(0)
                .validate()
                .is_err()
        );
        assert!(
            Query::new("select 1")
                .with_result_byte_limit(super::super::MAX_INTERACTIVE_RESULT_BYTES + 1)
                .validate()
                .is_err()
        );

        let mut bad_schema = Query::new("select 1");
        bad_schema.default_schema = Some("bad\nschema".into());
        assert!(bad_schema.validate().is_err());
    }

    #[test]
    fn edit_display_is_bounded_before_large_hex_or_json_allocations() {
        for value in [
            Value::Text("你".repeat(100)),
            Value::Bytes(vec![0xab; 100]),
            Value::Json(serde_json::json!({"items": vec!["value"; 100]})),
        ] {
            let (display, truncated) = value.display_for_edit_bounded(64);
            assert!(truncated);
            assert!(display.len() <= 64);
        }

        let (small, truncated) =
            Value::Json(serde_json::json!({"a": 1})).display_for_edit_bounded(128);
        assert!(!truncated);
        assert!(small.contains('\n'));
    }
}
