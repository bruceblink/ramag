//! CLI 纯函数层：命令行 → argv 分词（含引号）+ RedisValue → redis-cli 风格多行文本。
//! 无 UI、无 IO，全部可单测。

use ramag_domain::entities::RedisValue;

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
                    if !byte.is_ascii() {
                        return Err(
                            "当前命令行仅支持 UTF-8 参数，\\xHH 不能表示 80-FF 原始字节".into()
                        );
                    }
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

// 渲染层已行级虚拟化（uniform_list），上限只防内存失控。
// 总预算须大于单标量上限 + 引号等包装，否则 8 MiB 标量会整行被截断标记顶掉
const MAX_FORMAT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FORMAT_LINES: usize = 50_000;
const MAX_FORMAT_DEPTH: usize = 16;
/// 对齐值详情的 8 MiB 加载上限
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
            // 额外 -1 是部分行自身的换行开销，否则 finish 里加截断标记正好越界 1 字节，
            // 会把刚写入的唯一内容行再弹掉
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

/// 分段格式化结果：`cursor` 为续展开游标（标量=已消费字节偏移，顶层容器=已消费元素数），
/// None 表示已全部展开。原始值仍在内存（driver 已整体拉回），续展开无需重新查询
pub struct FormattedChunk {
    pub lines: Vec<String>,
    pub cursor: Option<usize>,
}

/// 标量续展开的单段原文字节数；escape 最坏 4 倍膨胀，段输出仍在 MAX_FORMAT_BYTES 内
const CONTINUE_SCALAR_CHUNK_BYTES: usize = 2 * 1024 * 1024;

/// 首次格式化：未超限走完整路径（含 JSON 美化）；超限返回首段 + 游标
pub fn lines_of_first(v: &RedisValue) -> FormattedChunk {
    format_chunk(v, 0)
}

/// 从游标继续格式化下一段（点击「继续展开」时调用）
pub fn lines_of_more(v: &RedisValue, cursor: usize) -> FormattedChunk {
    format_chunk(v, cursor)
}

fn format_chunk(v: &RedisValue, start: usize) -> FormattedChunk {
    match v {
        RedisValue::Text(s) if s.len() > MAX_SCALAR_INPUT_BYTES => text_chunk(s, start),
        RedisValue::Bytes(b) if b.len() > MAX_SCALAR_INPUT_BYTES => bytes_chunk(b, start),
        _ => match container_len(v) {
            Some(len) => container_chunk(v, len, start),
            // 未超限标量 / 小值：完整格式化，一次到位
            None => FormattedChunk {
                lines: lines_of(v),
                cursor: None,
            },
        },
    }
}

/// 超限文本分段：每段一条原文大行（escape 后），渲染层再按显示宽硬切。
/// 引号只在真正首尾；JSON 美化不适用于半截内容，超限值一律按原文展示
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

/// 顶层容器的元素数；非容器返回 None
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

/// 容器分段：从 start 元素起按元素原子填充，段预算即格式化总预算；
/// 单个超大元素独占一段（允许该段超预算，元素内部仍有自身截断保护）
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

/// 生成容器第 index 个元素的完整显示行（含全局编号与缩进）；
/// lines_of 全量路径与 chunk 分段路径共用，保证两种入口输出一致
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
            // 值单行时 inline 到 key 行；多行才另起缩进块
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

/// 容器全量路径共用骨架：逐元素生成行（与 chunk 分段路径同源）入预算 buffer
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
        assert!(tokenize(r#"SET key "\xFF""#).is_err());
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
    fn chunked_scalar_reassembles_full_value_via_cursor() {
        // 超限文本分段：沿游标取完所有段，拼回完整值（首尾带引号）
        let value = "y".repeat(MAX_SCALAR_INPUT_BYTES + MAX_SCALAR_INPUT_BYTES / 2);
        let v = RedisValue::Text(value.clone());
        let mut all = String::new();
        let mut chunk = lines_of_first(&v);
        let mut rounds = 0;
        loop {
            for line in &chunk.lines {
                all.push_str(line);
            }
            rounds += 1;
            match chunk.cursor {
                Some(cursor) => chunk = lines_of_more(&v, cursor),
                None => break,
            }
        }
        assert!(rounds > 1, "超限值应分成多段");
        assert_eq!(all, format!("\"{value}\""));
    }

    #[test]
    fn chunked_container_resumes_at_element_boundary_with_global_numbering() {
        // 容器分段：单元素放不下时停在元素边界，续段编号全局连续
        let big = "z".repeat(MAX_FORMAT_BYTES / 2);
        let items = vec![
            RedisValue::Text(big.clone()),
            RedisValue::Text(big),
            RedisValue::Int(7),
        ];
        let v = RedisValue::Array(items);
        let first = lines_of_first(&v);
        let cursor = first.cursor.expect("应有续展开游标");
        let more = lines_of_more(&v, cursor);
        let joined = more.lines.join("\n");
        assert!(joined.contains("3) (integer) 7"), "续段应保持全局编号");
    }

    #[test]
    fn max_scalar_keeps_content_instead_of_marker_only() {
        // 满上限的 8 MiB 标量必须保留内容行；此前预算 off-by-one 会把内容行弹光只剩截断标记
        let s = "x".repeat(MAX_SCALAR_INPUT_BYTES);
        let lines = lines_of(&RedisValue::Text(s));
        assert!(lines.iter().any(|line| line.len() > 1_000));
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
