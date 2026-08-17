//! Redis CLI 值的 redis-cli 风格多行格式化。

mod tokenize;

pub use tokenize::tokenize;

use ramag_domain::entities::RedisValue;

use crate::views::value_display::{self, ViewMode};

/// 将 Redis 值格式化为 redis-cli 风格多行文本。
pub fn lines_of(v: &RedisValue) -> Vec<String> {
    lines_of_inner(v, 0)
}

// 渲染已虚拟化；上限仅防内存失控并为截断标记预留空间。
const MAX_FORMAT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FORMAT_LINES: usize = 50_000;
const MAX_FORMAT_DEPTH: usize = 16;
/// 与值详情一致的单标量加载上限。
const MAX_SCALAR_INPUT_BYTES: usize = 8 * 1024 * 1024;
const TRUNCATION_LINE: &str = "… 响应过大，后续内容已截断";

struct LineBuffer {
    lines: Vec<String>,
    bytes: usize,
    truncated: bool,
}

impl LineBuffer {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            bytes: 0,
            truncated: false,
        }
    }

    fn push(&mut self, line: String) -> bool {
        if self.truncated || self.lines.len() >= MAX_FORMAT_LINES {
            self.truncated = true;
            return false;
        }
        let cost = line.len().saturating_add(1);
        if cost > MAX_FORMAT_BYTES.saturating_sub(self.bytes) {
            let marker_cost = TRUNCATION_LINE.len().saturating_add(1);
            // 预留截断标记空间，避免丢失唯一内容行。
            let available = MAX_FORMAT_BYTES
                .saturating_sub(self.bytes)
                .saturating_sub(marker_cost)
                .saturating_sub('…'.len_utf8())
                .saturating_sub(1);
            let prefix = utf8_prefix(&line, available);
            if !prefix.is_empty() {
                let partial = format!("{prefix}…");
                self.bytes = self.bytes.saturating_add(partial.len() + 1);
                self.lines.push(partial);
            }
            self.truncated = true;
            return false;
        }
        self.bytes += cost;
        self.lines.push(line);
        true
    }

    fn can_continue(&self) -> bool {
        !self.truncated
    }

    fn finish(mut self) -> Vec<String> {
        if self.truncated {
            let marker_cost = TRUNCATION_LINE.len() + 1;
            while self.lines.len() >= MAX_FORMAT_LINES
                || self.bytes.saturating_add(marker_cost) > MAX_FORMAT_BYTES
            {
                let Some(line) = self.lines.pop() else {
                    break;
                };
                self.bytes = self.bytes.saturating_sub(line.len() + 1);
            }
            if self.lines.len() < MAX_FORMAT_LINES
                && self.bytes.saturating_add(marker_cost) <= MAX_FORMAT_BYTES
            {
                self.lines.push(TRUNCATION_LINE.into());
            }
        }
        self.lines
    }
}

/// 分段格式化结果；cursor 为续展开位置，None 表示已完成。
pub struct FormattedChunk {
    pub lines: Vec<String>,
    pub cursor: Option<usize>,
}

/// 标量续展开的单段原文字节数。
const CONTINUE_SCALAR_CHUNK_BYTES: usize = 2 * 1024 * 1024;

/// 首次格式化；超限时返回首段和游标。
pub fn lines_of_first(v: &RedisValue) -> FormattedChunk {
    format_chunk(v, 0)
}

/// 从游标继续格式化。
pub fn lines_of_more(v: &RedisValue, cursor: usize) -> FormattedChunk {
    format_chunk(v, cursor)
}

fn format_chunk(v: &RedisValue, start: usize) -> FormattedChunk {
    match v {
        RedisValue::Text(s) if s.len() > MAX_SCALAR_INPUT_BYTES => text_chunk(s, start),
        RedisValue::Bytes(b) if b.len() > MAX_SCALAR_INPUT_BYTES => bytes_chunk(b, start),
        _ => match container_len(v) {
            Some(len) => container_chunk(v, len, start),
            // 小值一次完整格式化。
            None => FormattedChunk {
                lines: lines_of(v),
                cursor: None,
            },
        },
    }
}

/// 超限文本按原文分段；引号仅在首尾，半截内容不做 JSON 美化。
fn text_chunk(s: &str, start: usize) -> FormattedChunk {
    let total = s.len();
    let mut end = (start.saturating_add(CONTINUE_SCALAR_CHUNK_BYTES)).min(total);
    while !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut line = String::new();
    if start == 0 {
        line.push('"');
    }
    line.push_str(&escape_str(&s[start..end]));
    let done = end >= total;
    if done {
        line.push('"');
    }
    FormattedChunk {
        lines: vec![line],
        cursor: (!done).then_some(end),
    }
}

fn bytes_chunk(b: &[u8], start: usize) -> FormattedChunk {
    let total = b.len();
    let end = (start.saturating_add(CONTINUE_SCALAR_CHUNK_BYTES)).min(total);
    let mut line = String::new();
    if start == 0 {
        line.push('"');
    }
    line.push_str(&escape_bytes(&b[start..end]));
    let done = end >= total;
    if done {
        line.push('"');
    }
    FormattedChunk {
        lines: vec![line],
        cursor: (!done).then_some(end),
    }
}

/// 顶层容器的元素数；非容器为 None。
fn container_len(v: &RedisValue) -> Option<usize> {
    match v {
        RedisValue::List(items) | RedisValue::Set(items) | RedisValue::Array(items) => {
            Some(items.len())
        }
        RedisValue::Hash(pairs) => Some(pairs.len()),
        RedisValue::ZSet(pairs) => Some(pairs.len()),
        RedisValue::Stream(entries) => Some(entries.len()),
        _ => None,
    }
}

/// 容器按元素分段；单个超大元素独占一段。
fn container_chunk(v: &RedisValue, len: usize, start: usize) -> FormattedChunk {
    if len == 0 {
        return FormattedChunk {
            lines: vec!["(empty)".into()],
            cursor: None,
        };
    }
    let mut out = LineBuffer::new();
    let mut index = start;
    while index < len {
        let element = element_lines_for(v, index, 0);
        let element_cost: usize = element.iter().map(|line| line.len() + 1).sum();
        let fits = out.lines.len().saturating_add(element.len()) <= MAX_FORMAT_LINES
            && out.bytes.saturating_add(element_cost) <= MAX_FORMAT_BYTES;
        if !fits && !out.lines.is_empty() {
            break;
        }
        out.bytes = out.bytes.saturating_add(element_cost);
        out.lines.extend(element);
        index += 1;
    }
    FormattedChunk {
        cursor: (index < len).then_some(index),
        lines: out.lines,
    }
}

/// 生成容器元素的完整显示行，供完整和分段入口共用。
fn element_lines_for(v: &RedisValue, index: usize, depth: usize) -> Vec<String> {
    let head = format!("{}) ", index + 1);
    let pad = " ".repeat(head.len());
    match v {
        RedisValue::List(items) | RedisValue::Set(items) | RedisValue::Array(items) => {
            indented_lines(&head, &pad, lines_of_inner(&items[index], depth + 1))
        }
        RedisValue::Hash(pairs) => {
            let (k, val) = &pairs[index];
            let (key, key_truncated) = text_prefix(k, MAX_SCALAR_INPUT_BYTES);
            let key_part = format!(
                "\"{}{}\" => ",
                escape_str(key),
                if key_truncated { "…" } else { "" }
            );
            let vlines = lines_of_inner(val, depth + 1);
            // 单行值内联，多行值另起缩进块。
            if vlines.len() == 1 {
                let value = vlines.into_iter().next().unwrap_or_default();
                vec![format!("{head}{key_part}{value}")]
            } else {
                let mut lines = vec![format!("{head}{key_part}")];
                for line in vlines {
                    lines.push(format!("{pad}{line}"));
                }
                lines
            }
        }
        RedisValue::ZSet(pairs) => {
            let (member, score) = &pairs[index];
            lines_of_inner(member, depth + 1)
                .into_iter()
                .enumerate()
                .map(|(j, line)| {
                    if j == 0 {
                        format!("{head}{line} (score {score})")
                    } else {
                        format!("{pad}{line}")
                    }
                })
                .collect()
        }
        RedisValue::Stream(entries) => {
            let entry = &entries[index];
            let (id, id_truncated) = text_prefix(&entry.id, MAX_SCALAR_INPUT_BYTES);
            let mut lines = vec![format!("{head}{id}{}", if id_truncated { "…" } else { "" })];
            for (j, (k, value)) in entry.fields.iter().enumerate() {
                let (key, key_truncated) = text_prefix(k, MAX_SCALAR_INPUT_BYTES);
                let (value, value_truncated) = text_prefix(value, MAX_SCALAR_INPUT_BYTES);
                let mut key = escape_str(key);
                let mut value = escape_str(value);
                if key_truncated {
                    key.push('…');
                }
                if value_truncated {
                    value.push('…');
                }
                lines.push(format!("{pad}{}) \"{}\" => \"{}\"", j + 1, key, value));
            }
            lines
        }
        _ => Vec::new(),
    }
}

fn indented_lines(head: &str, pad: &str, child: Vec<String>) -> Vec<String> {
    child
        .into_iter()
        .enumerate()
        .map(|(j, line)| {
            if j == 0 {
                format!("{head}{line}")
            } else {
                format!("{pad}{line}")
            }
        })
        .collect()
}

fn lines_of_inner(v: &RedisValue, depth: usize) -> Vec<String> {
    if depth >= MAX_FORMAT_DEPTH {
        return vec!["… 嵌套层级过深，已停止展开".into()];
    }
    match v {
        RedisValue::Nil => vec!["(nil)".into()],
        RedisValue::Text(s) => text_lines(s),
        RedisValue::Int(i) => vec![format!("(integer) {i}")],
        RedisValue::Float(f) => vec![format!("(double) {f}")],
        RedisValue::Bool(b) => vec![format!("(boolean) {b}")],
        RedisValue::Bytes(b) => bytes_lines(b),
        RedisValue::List(items) | RedisValue::Set(items) | RedisValue::Array(items) => {
            container_lines(v, items.len(), depth)
        }
        RedisValue::Hash(pairs) => container_lines(v, pairs.len(), depth),
        RedisValue::ZSet(pairs) => container_lines(v, pairs.len(), depth),
        RedisValue::Stream(entries) => container_lines(v, entries.len(), depth),
    }
}

/// JSON 字符串多行美化，其余原样加引号。
fn text_lines(s: &str) -> Vec<String> {
    let (preview, truncated) = text_prefix(s, MAX_SCALAR_INPUT_BYTES);
    let mut out = LineBuffer::new();
    if matches!(
        value_display::auto_view_mode(preview.as_bytes()),
        ViewMode::Json
    ) {
        for line in value_display::render_text(preview, ViewMode::Json).lines() {
            if !out.push(line.to_string()) {
                break;
            }
        }
    } else {
        out.push(format!("\"{}\"", escape_str(preview)));
    }
    out.truncated |= truncated;
    out.finish()
}

fn bytes_lines(bytes: &[u8]) -> Vec<String> {
    let shown = &bytes[..bytes.len().min(MAX_SCALAR_INPUT_BYTES)];
    let mut out = LineBuffer::new();
    out.push(format!("\"{}\"", escape_bytes(shown)));
    out.truncated |= shown.len() < bytes.len();
    out.finish()
}

/// 容器完整格式化，逐元素写入预算缓冲区。
fn container_lines(v: &RedisValue, len: usize, depth: usize) -> Vec<String> {
    if len == 0 {
        return vec!["(empty)".into()];
    }
    let mut out = LineBuffer::new();
    for index in 0..len {
        if !out.can_continue() {
            break;
        }
        for line in element_lines_for(v, index, depth) {
            if !out.push(line) {
                break;
            }
        }
    }
    out.finish()
}

fn text_prefix(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    (utf8_prefix(text, limit), true)
}

fn utf8_prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &text[..end]
}

/// 显示用字符串转义。
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 二进制值转义：可打印 ASCII 原样，其余转 `\xHH`。
fn escape_bytes(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for &byte in b {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out
}

#[cfg(test)]
mod tests;
