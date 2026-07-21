//! 值显示：Raw / JSON / Hex / base64 视图 + Gzip 自动解压。仅作用于 String / Bytes 标量

use base64::Engine as _;
use flate2::read::GzDecoder;
use std::fmt;
use std::io::{self, Read, Write};

/// 行虚拟化展示的单行最大字符数：超长行硬切成段，保证等高行且避免单节点排版爆炸
pub const MAX_DISPLAY_LINE_CHARS: usize = 200;
/// 行虚拟化内容区固定宽度：200 字符 × ~9px（text_sm 等宽）+ 余量，
/// 配合外层横向滚动保证行尾不被视口裁掉
pub const DISPLAY_CONTENT_WIDTH_PX: f32 = 1840.0;

/// 按行切分显示文本；超长行按字符数硬切（char 边界安全），供 uniform_list 等高行虚拟化。
/// 大值塞进单个文本节点的整体排版是滚动卡死的根源。空文本给单个空行占位
pub fn split_display_lines(text: &str) -> Vec<gpui::SharedString> {
    let mut lines: Vec<gpui::SharedString> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.is_empty() {
            lines.push(gpui::SharedString::default());
            continue;
        }
        let mut start = 0usize;
        let mut count = 0usize;
        for (index, _) in raw.char_indices() {
            if count == MAX_DISPLAY_LINE_CHARS {
                lines.push(gpui::SharedString::from(raw[start..index].to_string()));
                start = index;
                count = 0;
            }
            count += 1;
        }
        lines.push(gpui::SharedString::from(raw[start..].to_string()));
    }
    if lines.is_empty() {
        lines.push(gpui::SharedString::default());
    }
    lines
}

#[cfg(test)]
mod split_lines_tests {
    use super::{MAX_DISPLAY_LINE_CHARS, split_display_lines};

    #[test]
    fn display_lines_split_by_newline_and_hard_wrap_on_char_boundary() {
        assert_eq!(split_display_lines("").len(), 1);
        assert_eq!(split_display_lines("a\nb").len(), 2);
        // 尾随换行保留末尾空行；\r\n 归一
        assert_eq!(split_display_lines("a\r\nb\n").len(), 3);
        // 超长行按字符数硬切，多字节字符不越界
        let long = "汉".repeat(MAX_DISPLAY_LINE_CHARS + 5);
        let lines = split_display_lines(&long);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chars().count(), MAX_DISPLAY_LINE_CHARS);
        assert_eq!(lines[1].chars().count(), 5);
    }
}

const MAX_GZIP_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_RENDER_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RENDERED_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const RENDER_HINT_RESERVE_BYTES: usize = 256;
const HEX_LINE_SOURCE_BYTES: usize = 16;
const HEX_LINE_RENDERED_BYTES: usize = 79;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// 原文（utf-8）
    #[default]
    Raw,
    /// JSON 解析 + pretty
    Json,
    /// Hex 字节流（每字节 2 位 + 空格分隔，每 16 字节换行）
    Hex,
    /// base64 编码
    Base64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GzipDecodeError {
    Invalid(String),
    TooLarge { limit: usize },
}

impl fmt::Display for GzipDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => write!(f, "压缩数据无效：{error}"),
            Self::TooLarge { limit } => {
                write!(f, "解压后超过 {} MiB 安全上限", limit / 1024 / 1024)
            }
        }
    }
}

/// Gzip magic：检测到 `1f 8b` 前缀才尝试解压，并限制输出大小以防压缩炸弹。
pub fn try_decompress_gzip(bytes: &[u8]) -> Result<Option<Vec<u8>>, GzipDecodeError> {
    decompress_gzip_with_limit(bytes, MAX_GZIP_DECOMPRESSED_BYTES)
}

fn decompress_gzip_with_limit(
    bytes: &[u8],
    limit: usize,
) -> Result<Option<Vec<u8>>, GzipDecodeError> {
    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
        return Ok(None);
    }
    let decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut out)
        .map_err(|error| GzipDecodeError::Invalid(error.to_string()))?;
    if out.len() > limit {
        return Err(GzipDecodeError::TooLarge { limit });
    }
    Ok(Some(out))
}

/// 以指定 ViewMode 渲染文本（Raw / JSON / Hex / base64）
pub fn render_text(text: &str, mode: ViewMode) -> String {
    render_text_with_limit(text, mode, MAX_RENDER_SOURCE_BYTES)
}

fn render_text_with_limit(text: &str, mode: ViewMode, limit: usize) -> String {
    render_text_with_limits(text, mode, limit, MAX_RENDERED_OUTPUT_BYTES)
}

fn render_text_with_limits(
    text: &str,
    mode: ViewMode,
    source_limit: usize,
    output_limit: usize,
) -> String {
    let source_limit = rendered_source_limit(mode, source_limit, output_limit, text.len());
    let (preview, truncated) = truncate_text_bytes(text, source_limit);
    let mut rendered = match mode {
        ViewMode::Raw => preview.to_string(),
        ViewMode::Json if truncated => {
            format!("(JSON 内容过大，未执行完整格式化)\n\n{preview}")
        }
        ViewMode::Json => pretty_json_with_limit(preview.as_bytes(), output_limit),
        ViewMode::Hex => to_hex_dump(preview.as_bytes()),
        ViewMode::Base64 => base64::engine::general_purpose::STANDARD.encode(preview.as_bytes()),
    };
    if truncated {
        append_truncation_hint(&mut rendered, preview.len(), text.len());
    }
    cap_rendered_output(rendered, output_limit)
}

/// 以指定 ViewMode 渲染字节流
pub fn render_bytes(bytes: &[u8], mode: ViewMode) -> String {
    render_bytes_with_limit(bytes, mode, MAX_RENDER_SOURCE_BYTES)
}

fn render_bytes_with_limit(bytes: &[u8], mode: ViewMode, limit: usize) -> String {
    render_bytes_with_limits(bytes, mode, limit, MAX_RENDERED_OUTPUT_BYTES)
}

fn render_bytes_with_limits(
    bytes: &[u8],
    mode: ViewMode,
    source_limit: usize,
    output_limit: usize,
) -> String {
    let source_limit = rendered_source_limit(mode, source_limit, output_limit, bytes.len());
    let preview = &bytes[..bytes.len().min(source_limit)];
    let truncated = preview.len() < bytes.len();
    let mut rendered = match mode {
        ViewMode::Raw => match utf8_preview(preview) {
            Ok(s) => s.to_string(),
            Err(_) => format!("[{} bytes：非 UTF-8，请切到 Hex/base64 查看]", bytes.len()),
        },
        ViewMode::Json if truncated => match utf8_preview(preview) {
            Ok(text) => format!("(JSON 内容过大，未执行完整格式化)\n\n{text}"),
            Err(_) => format!("[{} bytes：非 UTF-8，无法解析为 JSON]", bytes.len()),
        },
        ViewMode::Json => pretty_json_with_limit(preview, output_limit),
        ViewMode::Hex => to_hex_dump(preview),
        ViewMode::Base64 => base64::engine::general_purpose::STANDARD.encode(preview),
    };
    if truncated {
        append_truncation_hint(&mut rendered, preview.len(), bytes.len());
    }
    cap_rendered_output(rendered, output_limit)
}

fn rendered_source_limit(
    mode: ViewMode,
    source_limit: usize,
    output_limit: usize,
    source_bytes: usize,
) -> usize {
    let content_limit = output_limit.saturating_sub(RENDER_HINT_RESERVE_BYTES);
    match mode {
        ViewMode::Hex => source_limit
            .min((content_limit / HEX_LINE_RENDERED_BYTES).saturating_mul(HEX_LINE_SOURCE_BYTES)),
        ViewMode::Base64 => source_limit.min((content_limit / 4).saturating_mul(3)),
        ViewMode::Raw | ViewMode::Json if source_bytes > source_limit => {
            source_limit.min(content_limit)
        }
        ViewMode::Raw | ViewMode::Json => source_limit,
    }
}

fn truncate_text_bytes(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (&text[..end], true)
}

fn utf8_preview(bytes: &[u8]) -> Result<&str, std::str::Utf8Error> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
        }
        Err(error) => Err(error),
    }
}

fn append_truncation_hint(rendered: &mut String, shown: usize, total: usize) {
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str(&format!("\n[内容过大，仅显示前 {shown} / {total} bytes]"));
}

fn cap_rendered_output(mut rendered: String, limit: usize) -> String {
    if rendered.len() <= limit {
        return rendered;
    }
    append_bounded_hint(&mut rendered, limit, "渲染结果过大，仅显示前缀");
    rendered
}

fn append_bounded_hint(rendered: &mut String, limit: usize, message: &str) {
    let hint = format!("\n\n[{message}]");
    let content_limit = limit.saturating_sub(hint.len());
    truncate_string_bytes(rendered, content_limit);
    rendered.push_str(&hint);
    truncate_string_bytes(rendered, limit);
}

fn truncate_string_bytes(value: &mut String, limit: usize) {
    if value.len() <= limit {
        return;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

/// 按内容判定默认视图：内容本身、或被编码成字符串的内容（兼容二次编码）解析后是 JSON
/// 对象/数组 → JSON（美化），否则 Raw。超过 256KB 不自动解析（默认 Raw，仍可手动切 JSON）
pub fn auto_view_mode(bytes: &[u8]) -> ViewMode {
    const MAX_AUTO_PARSE: usize = 256 * 1024;
    if bytes.len() > MAX_AUTO_PARSE {
        return ViewMode::Raw;
    }
    // 廉价前缀过滤：只有 { [ " 开头才值得解析（覆盖普通 JSON 与被字符串编码的 JSON）
    let Ok(text) = std::str::from_utf8(bytes) else {
        return ViewMode::Raw;
    };
    if !matches!(
        text.trim_start().as_bytes().first().copied(),
        Some(b'{' | b'[' | b'"')
    ) {
        return ViewMode::Raw;
    }
    // 解析（并解开字符串编码）后是对象/数组才默认 JSON。matches! 作用于临时 Result，不移动绑定变量
    let parsed =
        serde_json::from_slice::<serde_json::Value>(bytes).map(|v| unwrap_encoded_json(v, 4));
    if matches!(
        parsed,
        Ok(serde_json::Value::Object(_) | serde_json::Value::Array(_))
    ) {
        ViewMode::Json
    } else {
        ViewMode::Raw
    }
}

/// 解开被编码成字符串的 JSON（兼容二次编码，如 `"{\"a\":1}"`）：某层是字符串且其内容能解析为
/// JSON 对象/数组时继续解开，最多 depth 层防御。普通字符串/标量原样返回
fn unwrap_encoded_json(v: serde_json::Value, depth: u8) -> serde_json::Value {
    if depth == 0 {
        return v;
    }
    if let serde_json::Value::String(s) = &v
        && let Ok(inner) = serde_json::from_str::<serde_json::Value>(s)
        && matches!(
            inner,
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
        )
    {
        return unwrap_encoded_json(inner, depth - 1);
    }
    v
}

/// 解析 bytes 为 JSON 并 pretty 输出（先解开被字符串编码的 JSON）；失败时返回原文 + 提示
#[cfg(test)]
fn pretty_json(bytes: &[u8]) -> String {
    pretty_json_with_limit(bytes, MAX_RENDERED_OUTPUT_BYTES)
}

fn pretty_json_with_limit(bytes: &[u8], output_limit: usize) -> String {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => {
            let v = unwrap_encoded_json(v, 4);
            let mut writer = BoundedJsonWriter::new(output_limit);
            match serde_json::to_writer_pretty(&mut writer, &v) {
                Ok(()) => writer.into_string(),
                Err(_) if writer.exceeded => {
                    let mut rendered = writer.into_string();
                    append_bounded_hint(&mut rendered, output_limit, "格式化结果过大，仅显示前缀");
                    rendered
                }
                Err(_) => "(JSON 序列化失败)".to_string(),
            }
        }
        Err(e) => {
            let preview = std::str::from_utf8(bytes).unwrap_or("（非 UTF-8）");
            cap_rendered_output(format!("(无法解析为 JSON：{e})\n\n{preview}"), output_limit)
        }
    }
}

struct BoundedJsonWriter {
    output: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            output: Vec::with_capacity(limit.min(64 * 1024)),
            limit,
            exceeded: false,
        }
    }

    fn into_string(mut self) -> String {
        if let Err(error) = std::str::from_utf8(&self.output) {
            self.output.truncate(error.valid_up_to());
        }
        String::from_utf8(self.output).unwrap_or_default()
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.exceeded {
            return Err(io::Error::other("JSON output limit exceeded"));
        }
        let remaining = self.limit.saturating_sub(self.output.len());
        if remaining == 0 {
            self.exceeded = true;
            return Err(io::Error::other("JSON output limit exceeded"));
        }
        let written = remaining.min(bytes.len());
        self.output.extend_from_slice(&bytes[..written]);
        if written < bytes.len() {
            self.exceeded = true;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 经典 hex dump：每 16 字节一行；左侧偏移地址，右侧 ASCII 预览
fn to_hex_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(4));
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let offset = i * 16;
        out.push_str(&format!("{offset:08x}  "));
        // hex 部分（每字节 2 位 + 空格；不足 16 字节用空格补齐对齐）
        for (j, b) in chunk.iter().enumerate() {
            out.push_str(&format!("{b:02x} "));
            if j == 7 {
                out.push(' ');
            }
        }
        for j in chunk.len()..16 {
            out.push_str("   ");
            if j == 7 {
                out.push(' ');
            }
        }
        // ASCII 部分
        out.push_str(" |");
        for b in chunk {
            let c = if (0x20..0x7f).contains(b) {
                *b as char
            } else {
                '.'
            };
            out.push(c);
        }
        out.push_str("|\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_detect_and_decompress() {
        // gzip 编码 "hello world"
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(b"hello world").unwrap();
        let compressed = enc.finish().unwrap();

        let out = try_decompress_gzip(&compressed).unwrap().unwrap();
        assert_eq!(&out, b"hello world");
    }

    #[test]
    fn gzip_non_gzip_returns_none() {
        assert!(try_decompress_gzip(b"not gzip").unwrap().is_none());
        assert!(try_decompress_gzip(&[0x1f]).unwrap().is_none()); // 太短
    }

    #[test]
    fn gzip_output_is_bounded() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&[b'a'; 32]).unwrap();
        let compressed = enc.finish().unwrap();

        assert_eq!(
            decompress_gzip_with_limit(&compressed, 8),
            Err(GzipDecodeError::TooLarge { limit: 8 })
        );
    }

    #[test]
    fn malformed_gzip_returns_explicit_error() {
        assert!(matches!(
            try_decompress_gzip(&[0x1f, 0x8b, 0x00]),
            Err(GzipDecodeError::Invalid(_))
        ));
    }

    #[test]
    fn pretty_json_valid() {
        let out = pretty_json(br#"{"a":1,"b":[2,3]}"#);
        assert!(out.contains("\n  \"a\": 1"));
    }

    #[test]
    fn pretty_json_invalid_returns_preview() {
        let out = pretty_json(b"not json");
        assert!(out.contains("无法解析"));
        assert!(out.contains("not json"));
    }

    #[test]
    fn hex_dump_format() {
        let out = to_hex_dump(b"AB12");
        // "00000000  41 42 31 32                                       |AB12|"
        assert!(out.starts_with("00000000  41 42 31 32"));
        assert!(out.contains("|AB12|"));
    }

    #[test]
    fn render_text_modes() {
        assert_eq!(render_text("hi", ViewMode::Raw), "hi");
        assert_eq!(render_text("hi", ViewMode::Base64), "aGk=");
    }

    #[test]
    fn render_bytes_non_utf8_raw() {
        let s = render_bytes(&[0xff, 0xfe], ViewMode::Raw);
        assert!(s.contains("非 UTF-8"));
    }

    #[test]
    fn render_limit_is_utf8_safe_and_visible() {
        let rendered = render_text_with_limit("你好世界", ViewMode::Raw, 4);
        assert!(rendered.starts_with('你'));
        assert!(!rendered.starts_with("你好"));
        assert!(rendered.contains("仅显示前 3 / 12 bytes"));
    }

    #[test]
    fn expanded_views_only_process_bounded_prefix() {
        let rendered = render_bytes_with_limit(b"abcdef", ViewMode::Base64, 2);
        assert!(rendered.starts_with("YWI="));
        assert!(rendered.contains("仅显示前 2 / 6 bytes"));

        let json = render_text_with_limit(r#"{"a":123}"#, ViewMode::Json, 4);
        assert!(json.contains("未执行完整格式化"));
        assert!(json.contains("仅显示前"));
    }

    #[test]
    fn expanded_rendered_output_is_bounded() {
        let bytes = vec![b'a'; 1024];
        for mode in [ViewMode::Hex, ViewMode::Base64] {
            let rendered = render_bytes_with_limits(&bytes, mode, bytes.len(), 256);
            assert!(rendered.len() <= 256, "mode={mode:?}");
            assert!(rendered.contains("仅显示前"), "mode={mode:?}");
        }

        let json = format!("[{}]", vec!["0"; 300].join(","));
        let rendered = render_text_with_limits(&json, ViewMode::Json, json.len(), 256);
        assert!(rendered.len() <= 256);
        assert!(rendered.contains("格式化结果过大"));
    }

    #[test]
    fn auto_view_mode_detects_json_else_raw() {
        assert_eq!(auto_view_mode(br#"{"a":1}"#), ViewMode::Json);
        assert_eq!(auto_view_mode(b"[1,2,3]"), ViewMode::Json);
        // 前导空白也应识别为 JSON
        assert_eq!(auto_view_mode(b"  \n {\"a\":1}"), ViewMode::Json);
        // 被字符串编码的 JSON（二次编码）也应识别为 JSON
        let encoded = serde_json::to_string(r#"{"a":1}"#).unwrap();
        assert_eq!(auto_view_mode(encoded.as_bytes()), ViewMode::Json);
        // 纯文本 / 普通带引号字符串 / 非 UTF-8 / 空 → Raw
        assert_eq!(auto_view_mode(b"hello world"), ViewMode::Raw);
        assert_eq!(auto_view_mode(br#""hello""#), ViewMode::Raw);
        assert_eq!(auto_view_mode(&[0xff, 0xfe]), ViewMode::Raw);
        assert_eq!(auto_view_mode(b""), ViewMode::Raw);
    }

    #[test]
    fn pretty_json_unwraps_string_encoded() {
        // 值本身是 JSON 字符串，内容又是 JSON 对象（二次编码）→ 应解开并美化内层
        let encoded = serde_json::to_string(r#"{"a":1}"#).unwrap();
        let out = pretty_json(encoded.as_bytes());
        assert!(out.contains("\n  \"a\": 1"), "got: {out}");
        // 普通带引号字符串内容非 JSON → 原样（仍是带引号字符串，不强行展开）
        let plain = serde_json::to_string("hello").unwrap();
        assert_eq!(pretty_json(plain.as_bytes()), "\"hello\"");
    }
}
