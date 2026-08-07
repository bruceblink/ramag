use super::*;

pub(super) fn escaped_sql_literal_len(
    bytes: &[u8],
    is_pg: bool,
    max_bytes: usize,
) -> Option<usize> {
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

pub(super) fn json_sql_literal_len(
    value: &serde_json::Value,
    is_pg: bool,
    max_bytes: usize,
) -> Option<usize> {
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

pub(super) fn json_contains_query(value: &serde_json::Value, query_lower: &str) -> bool {
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

pub(super) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        output.push(char::from(DIGITS[(byte >> 4) as usize]));
        output.push(char::from(DIGITS[(byte & 0x0f) as usize]));
    }
    output
}

pub(super) fn bounded_hex(bytes: &[u8], max_bytes: usize) -> (String, bool) {
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

pub(super) fn bounded_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let end = floor_char_boundary(text, max_bytes);
    (
        with_truncation_notice(text[..end].to_string(), max_bytes),
        true,
    )
}

pub(super) fn with_truncation_notice(mut text: String, max_bytes: usize) -> String {
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

pub(super) fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub(super) fn serialize_json_prefix(
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
pub(super) fn escape_sql_string(s: &str, is_pg: bool) -> String {
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

pub(super) fn truncate(s: &str, max_len: usize) -> String {
    let mut chars = s.chars();
    let mut prefix: String = chars.by_ref().take(max_len).collect();
    if chars.next().is_some() {
        prefix.push('…');
    }
    prefix
}

/// GPUI 单行文本不接受换行符；此处理不影响完整取值接口。
pub(super) fn sanitize_inline(s: &str) -> String {
    if s.contains(['\n', '\r']) {
        s.replace(['\n', '\r'], " ")
    } else {
        s.to_string()
    }
}
