//! CLI 纯函数层：命令行 → argv 分词（含引号）+ RedisValue → redis-cli 风格多行文本。
//! 无 UI、无 IO，全部可单测。

use ramag_domain::entities::{RedisValue, StreamEntry};

use crate::views::value_display::{self, ViewMode};

/// 把一行命令切成 argv，仿 redis-cli sdssplitargs：
/// - 空白分隔；`"双引号"` 支持 `\n \r \t \xHH \" \\` 转义；`'单引号'` 仅 `\'` 转义、余原样
/// - 引号未闭合返回 Err（供前端就地提示，不发后端）
pub fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        // 一个 token 可由裸段与引号段拼接（如 foo"bar" → foobar）
        let mut cur = String::new();
        loop {
            match chars.peek().copied() {
                None => break,
                Some(c) if c.is_whitespace() => break,
                Some('"') => {
                    chars.next();
                    parse_double_quoted(&mut chars, &mut cur)?;
                }
                Some('\'') => {
                    chars.next();
                    parse_single_quoted(&mut chars, &mut cur)?;
                }
                Some(c) => {
                    cur.push(c);
                    chars.next();
                }
            }
        }
        args.push(cur);
    }
    Ok(args)
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// 双引号内：处理转义直到下一个未转义的 `"`
fn parse_double_quoted(chars: &mut Chars, out: &mut String) -> Result<(), String> {
    loop {
        match chars.next() {
            None => return Err("双引号未闭合".into()),
            Some('"') => return Ok(()),
            Some('\\') => match chars.next() {
                None => return Err("双引号未闭合".into()),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('x') => {
                    let h1 = chars.next().ok_or("\\x 需两位十六进制")?;
                    let h2 = chars.next().ok_or("\\x 需两位十六进制")?;
                    let high =
                        hex_nibble(h1).ok_or_else(|| "\\x 后须为两位十六进制".to_string())?;
                    let low = hex_nibble(h2).ok_or_else(|| "\\x 后须为两位十六进制".to_string())?;
                    let byte = (high << 4) | low;
                    out.push(byte as char);
                }
                Some(other) => out.push(other),
            },
            Some(c) => out.push(c),
        }
    }
}

fn hex_nibble(ch: char) -> Option<u8> {
    match ch {
        '0'..='9' => Some(ch as u8 - b'0'),
        'a'..='f' => Some(ch as u8 - b'a' + 10),
        'A'..='F' => Some(ch as u8 - b'A' + 10),
        _ => None,
    }
}

/// 单引号内：原样直到下一个 `'`，仅 `\'` 转义为 `'`
fn parse_single_quoted(chars: &mut Chars, out: &mut String) -> Result<(), String> {
    loop {
        match chars.next() {
            None => return Err("单引号未闭合".into()),
            Some('\'') => return Ok(()),
            Some('\\') if matches!(chars.peek(), Some('\'')) => {
                chars.next();
                out.push('\'');
            }
            Some(c) => out.push(c),
        }
    }
}

/// RedisValue → 多行文本（仿 redis-cli），每行相对本层左对齐；嵌套由父层缩进。
/// 标量返回单行；聚合用 `N)` 编号并递归缩进。
pub fn lines_of(v: &RedisValue) -> Vec<String> {
    lines_of_inner(v, 0)
}

const MAX_FORMAT_BYTES: usize = 1024 * 1024;
const MAX_FORMAT_LINES: usize = 2_000;
const MAX_FORMAT_DEPTH: usize = 16;
const MAX_SCALAR_INPUT_BYTES: usize = 256 * 1024;
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
            let available = MAX_FORMAT_BYTES
                .saturating_sub(self.bytes)
                .saturating_sub(marker_cost)
                .saturating_sub('…'.len_utf8());
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
            seq_lines(items, depth)
        }
        RedisValue::Hash(pairs) => hash_lines(pairs, depth),
        RedisValue::ZSet(pairs) => zset_lines(pairs, depth),
        RedisValue::Stream(entries) => stream_lines(entries),
    }
}

/// String 值：内容是 JSON（含被字符串编码的 JSON）则多行美化，否则原样加引号
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

/// 把子节点多行接到 `head` 之后：首行带 head，续行补等宽缩进
fn append_indented(out: &mut LineBuffer, head: &str, child: Vec<String>) {
    let pad = " ".repeat(head.len());
    for (j, line) in child.into_iter().enumerate() {
        let line = if j == 0 {
            format!("{head}{line}")
        } else {
            format!("{pad}{line}")
        };
        if !out.push(line) {
            break;
        }
    }
}

fn seq_lines(items: &[RedisValue], depth: usize) -> Vec<String> {
    if items.is_empty() {
        return vec!["(empty)".into()];
    }
    let mut out = LineBuffer::new();
    for (i, x) in items.iter().enumerate() {
        if !out.can_continue() {
            break;
        }
        append_indented(
            &mut out,
            &format!("{}) ", i + 1),
            lines_of_inner(x, depth + 1),
        );
    }
    out.finish()
}

fn hash_lines(pairs: &[(String, RedisValue)], depth: usize) -> Vec<String> {
    if pairs.is_empty() {
        return vec!["(empty)".into()];
    }
    let mut out = LineBuffer::new();
    for (i, (k, val)) in pairs.iter().enumerate() {
        if !out.can_continue() {
            break;
        }
        let vlines = lines_of_inner(val, depth + 1);
        let (key, key_truncated) = text_prefix(k, MAX_SCALAR_INPUT_BYTES);
        let key = format!(
            "\"{}{}\" => ",
            escape_str(key),
            if key_truncated { "…" } else { "" }
        );
        let head = format!("{}) ", i + 1);
        if vlines.len() == 1 {
            out.push(format!("{head}{key}{}", vlines[0]));
        } else {
            // 聚合值：键名独占一行，值整体降一层缩进
            if !out.push(format!("{head}{key}")) {
                break;
            }
            let pad = " ".repeat(head.len());
            for line in vlines {
                if !out.push(format!("{pad}{line}")) {
                    break;
                }
            }
        }
    }
    out.finish()
}

fn zset_lines(pairs: &[(RedisValue, f64)], depth: usize) -> Vec<String> {
    if pairs.is_empty() {
        return vec!["(empty)".into()];
    }
    let mut out = LineBuffer::new();
    for (i, (m, score)) in pairs.iter().enumerate() {
        if !out.can_continue() {
            break;
        }
        let head = format!("{}) ", i + 1);
        let pad = " ".repeat(head.len());
        for (j, line) in lines_of_inner(m, depth + 1).into_iter().enumerate() {
            let line = if j == 0 {
                format!("{head}{line} (score {score})")
            } else {
                format!("{pad}{line}")
            };
            if !out.push(line) {
                break;
            }
        }
    }
    out.finish()
}

fn stream_lines(entries: &[StreamEntry]) -> Vec<String> {
    if entries.is_empty() {
        return vec!["(empty)".into()];
    }
    let mut out = LineBuffer::new();
    for (i, e) in entries.iter().enumerate() {
        if !out.can_continue() {
            break;
        }
        let head = format!("{}) ", i + 1);
        let pad = " ".repeat(head.len());
        let (id, id_truncated) = text_prefix(&e.id, MAX_SCALAR_INPUT_BYTES);
        if !out.push(format!("{head}{id}{}", if id_truncated { "…" } else { "" })) {
            break;
        }
        for (j, (k, v)) in e.fields.iter().enumerate() {
            let (key, key_truncated) = text_prefix(k, MAX_SCALAR_INPUT_BYTES);
            let (value, value_truncated) = text_prefix(v, MAX_SCALAR_INPUT_BYTES);
            let mut key = escape_str(key);
            let mut value = escape_str(value);
            if key_truncated {
                key.push('…');
            }
            if value_truncated {
                value.push('…');
            }
            if !out.push(format!("{pad}{}) \"{}\" => \"{}\"", j + 1, key, value)) {
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

/// 显示用转义：引号/反斜杠/控制符转义，可打印字符原样
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

/// 二进制值转义：可打印 ASCII 原样，其余转 `\xHH`（仿 redis-cli）
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
mod tests {
    use super::*;

    #[test]
    fn tokenize_plain() {
        assert_eq!(tokenize("GET foo").unwrap(), vec!["GET", "foo"]);
        assert_eq!(tokenize("  PING  ").unwrap(), vec!["PING"]);
        assert!(tokenize("   ").unwrap().is_empty());
    }

    #[test]
    fn tokenize_quoted() {
        assert_eq!(
            tokenize(r#"SET k "a b c""#).unwrap(),
            vec!["SET", "k", "a b c"]
        );
        assert_eq!(tokenize(r#"SET k 'a b'"#).unwrap(), vec!["SET", "k", "a b"]);
        // 双引号内转义
        assert_eq!(
            tokenize(r#"SET k "a\tb""#).unwrap(),
            vec!["SET", "k", "a\tb"]
        );
        // 裸段与引号段拼接
        assert_eq!(tokenize(r#"foo"bar""#).unwrap(), vec!["foobar"]);
    }

    #[test]
    fn tokenize_unbalanced() {
        assert!(tokenize(r#"SET k "unclosed"#).is_err());
        assert!(tokenize("SET k 'unclosed").is_err());
    }

    #[test]
    fn tokenize_hex_escape_without_temporary_string() {
        assert_eq!(
            tokenize(r#"SET key "\x41\x7a\x2F""#).unwrap(),
            vec!["SET", "key", "Az/"]
        );
        assert!(tokenize(r#"SET key "\xG0""#).is_err());
    }

    #[test]
    fn format_scalars() {
        assert_eq!(lines_of(&RedisValue::Nil), vec!["(nil)"]);
        assert_eq!(lines_of(&RedisValue::Int(42)), vec!["(integer) 42"]);
        assert_eq!(lines_of(&RedisValue::Text("bar".into())), vec!["\"bar\""]);
    }

    #[test]
    fn format_text_json_pretty() {
        // JSON String 值应多行美化（非单行加引号）
        let lines = lines_of(&RedisValue::Text(r#"{"a":1,"b":2}"#.into()));
        assert!(lines.len() > 1, "JSON 应多行: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("\"a\"")));
    }

    #[test]
    fn format_bytes_hex() {
        let v = RedisValue::Bytes(vec![0xac, 0x41, 0x00]);
        assert_eq!(lines_of(&v), vec!["\"\\xacA\\x00\""]);
    }

    #[test]
    fn format_nested_array() {
        let v = RedisValue::Array(vec![
            RedisValue::Text("a".into()),
            RedisValue::Array(vec![RedisValue::Int(1), RedisValue::Int(2)]),
        ]);
        let lines = lines_of(&v);
        assert_eq!(
            lines,
            vec!["1) \"a\"", "2) 1) (integer) 1", "   2) (integer) 2"]
        );
    }

    #[test]
    fn format_hash_inline() {
        let v = RedisValue::Hash(vec![("f".into(), RedisValue::Text("v".into()))]);
        assert_eq!(lines_of(&v), vec!["1) \"f\" => \"v\""]);
    }

    #[test]
    fn format_empty() {
        assert_eq!(lines_of(&RedisValue::Array(vec![])), vec!["(empty)"]);
    }

    #[test]
    fn formatting_is_bounded_by_size_depth_and_lines() {
        let huge = RedisValue::Bytes(vec![0xff; MAX_SCALAR_INPUT_BYTES + 1]);
        let output = lines_of(&huge).join("\n");
        assert!(output.len() <= MAX_FORMAT_BYTES);
        assert!(output.contains(TRUNCATION_LINE));

        let many = RedisValue::Array(vec![RedisValue::Int(1); MAX_FORMAT_LINES + 1]);
        let lines = lines_of(&many);
        assert!(lines.len() <= MAX_FORMAT_LINES);
        assert_eq!(lines.last().map(String::as_str), Some(TRUNCATION_LINE));

        let mut deep = RedisValue::Nil;
        for _ in 0..=MAX_FORMAT_DEPTH {
            deep = RedisValue::Array(vec![deep]);
        }
        assert!(lines_of(&deep).join("\n").contains("嵌套层级过深"));
    }
}
