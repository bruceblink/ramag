//! 剪贴板历史、设置和采集数据。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipId(pub Uuid);

impl ClipId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ClipId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ClipId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipKind {
    Text,
    Link,
    Color,
    Image,
    Files,
}

impl ClipKind {
    pub fn label(&self) -> &'static str {
        match self {
            ClipKind::Text => "文本",
            ClipKind::Link => "链接",
            ClipKind::Color => "颜色",
            ClipKind::Image => "图片",
            ClipKind::Files => "文件",
        }
    }

    pub fn label_en(&self) -> &'static str {
        match self {
            ClipKind::Text => "Text",
            ClipKind::Link => "Link",
            ClipKind::Color => "Color",
            ClipKind::Image => "Image",
            ClipKind::Files => "Files",
        }
    }

    pub fn all() -> &'static [ClipKind] {
        &[
            ClipKind::Text,
            ClipKind::Link,
            ClipKind::Color,
            ClipKind::Image,
            ClipKind::Files,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipSource {
    pub bundle_id: String,
    pub name: String,
}

/// 文本加密入库，图片仅保存加密文件路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipItem {
    pub id: ClipId,
    pub kind: ClipKind,
    pub text: Option<String>,
    /// 富文本 RTF 原始数据（伴随 text；粘贴时与纯文本一起写回）
    #[serde(default)]
    pub rtf: Option<Vec<u8>>,
    /// Image 类型的原图落盘路径（AES 加密密文）
    pub image_path: Option<String>,
    /// Image 类型的缩略图落盘路径（AES 加密密文，列表展示用，降解码成本）
    #[serde(default)]
    pub thumb_path: Option<String>,
    pub image_dims: Option<(u32, u32)>,
    #[serde(default)]
    pub files: Vec<String>,
    pub preview: String,
    pub source: Option<ClipSource>,
    pub byte_size: u64,
    /// 内容指纹（fnv1a 十六进制），同内容去重
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}

impl ClipItem {
    /// 当前条目直接常驻内存的原始负载大小；图片正文只保存磁盘路径，因此不计 `byte_size`。
    pub fn inline_payload_bytes(&self) -> u64 {
        let text = self.text.as_ref().map_or(0, String::len);
        let rtf = self.rtf.as_ref().map_or(0, Vec::len);
        let files = self
            .files
            .iter()
            .fold(0usize, |total, path| total.saturating_add(path.len()));
        u64::try_from(text.saturating_add(rtf).saturating_add(files)).unwrap_or(u64::MAX)
    }
}

#[derive(Debug)]
pub struct ClipSearchResult {
    pub items: Vec<ClipItem>,
    pub truncated: bool,
}

/// 单条剪贴内容允许的最大持久化体积；避免异常设置放大采集时的内存与磁盘占用。
pub const MAX_CLIPBOARD_ITEM_BYTES: u64 = 64 * 1024 * 1024;
/// 全量历史搜索会逐条解密与匹配，异常长查询词只会放大比较成本。
pub const MAX_CLIPBOARD_SEARCH_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipboardSettings {
    pub enabled: bool,
    pub capture_images: bool,
    pub max_item_bytes: u64,
    /// 抽屉选中后自动粘贴（平台可能需要系统权限；false 仅复制）
    pub auto_paste: bool,
    /// 全局热键改用主修饰键+Alt+V（默认 Shift 组合与部分应用「粘贴为纯文本」冲突时切换）。
    /// serde 默认：旧版持久化 JSON 缺此字段时不整体回退默认设置
    #[serde(default)]
    pub alternate_hotkey: bool,
}

impl ClipboardSettings {
    /// 校验影响采集资源消耗的设置；持久化输入不可信，载入与保存都必须经过此边界。
    pub fn validate(&self) -> Result<(), String> {
        if self.max_item_bytes > MAX_CLIPBOARD_ITEM_BYTES {
            return Err(format!(
                "单条剪贴内容上限过大：{} > {MAX_CLIPBOARD_ITEM_BYTES}",
                self.max_item_bytes
            ));
        }
        Ok(())
    }
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            capture_images: true,
            max_item_bytes: 10 * 1024 * 1024,
            auto_paste: true,
            alternate_hotkey: false,
        }
    }
}

/// 尚未分类或持久化的原始采集内容。
#[derive(Debug, Clone, Default)]
pub struct CapturedClip {
    pub text: Option<String>,
    pub rtf: Option<Vec<u8>>,
    pub image_png: Option<Vec<u8>>,
    pub image_dims: Option<(u32, u32)>,
    pub files: Vec<String>,
    /// 带平台敏感/临时内容标记（密码管理器等），不应记录
    pub concealed: bool,
}

/// fnv1a-64 内容指纹。std Hasher 不保证跨编译器版本稳定，落盘指纹必须自实现
pub fn fnv1a_hash(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

pub fn classify_text(s: &str) -> ClipKind {
    let t = s.trim();
    if is_safe_http_url(t) {
        ClipKind::Link
    } else if is_color(t) {
        ClipKind::Color
    } else {
        ClipKind::Text
    }
}

pub fn is_safe_http_url(value: &str) -> bool {
    const MAX_URL_BYTES: usize = 16 * 1024;

    let value = value.trim();
    if value.len() > MAX_URL_BYTES
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")) && value.len() > 10
}

fn is_color(t: &str) -> bool {
    // 十六进制格式：#RGB / #RGBA / #RRGGBB / #RRGGBBAA。
    if let Some(hex) = t.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    // 函数格式：rgb(...) / rgba(...) / hsl(...) / hsla(...)。
    let lower = t.to_ascii_lowercase();
    for prefix in ["rgb(", "rgba(", "hsl(", "hsla("] {
        if lower.starts_with(prefix) && lower.ends_with(')') {
            return true;
        }
    }
    false
}

/// 颜色文本 → RGB。仅支持 #hex 形态（UI 色卡预览用），其余返回 None
pub fn parse_hex_color(t: &str) -> Option<(u8, u8, u8)> {
    let hex = t.trim().strip_prefix('#')?;
    let expand = |c: char| -> Option<u8> {
        let v = c.to_digit(16)? as u8;
        Some(v << 4 | v)
    };
    let byte_at = |i: usize| -> Option<u8> { u8::from_str_radix(hex.get(i..i + 2)?, 16).ok() };
    match hex.len() {
        3 | 4 => {
            let mut cs = hex.chars();
            Some((
                expand(cs.next()?)?,
                expand(cs.next()?)?,
                expand(cs.next()?)?,
            ))
        }
        6 | 8 => Some((byte_at(0)?, byte_at(2)?, byte_at(4)?)),
        _ => None,
    }
}

pub fn make_preview(
    kind: ClipKind,
    text: Option<&str>,
    files: &[String],
    dims: Option<(u32, u32)>,
) -> String {
    const MAX: usize = 120;
    match kind {
        ClipKind::Image => match dims {
            Some((w, h)) => format!("图片 {w}×{h}"),
            None => "图片".to_string(),
        },
        ClipKind::Files => match files {
            [] => "文件".to_string(),
            [one] => file_name(one),
            [first, ..] => format!("{} 等 {} 个文件", file_name(first), files.len()),
        },
        _ => {
            let line = text
                .unwrap_or_default()
                .trim()
                .lines()
                .next()
                .unwrap_or_default();
            line.char_indices()
                .nth(MAX)
                .map_or_else(|| line.to_string(), |(end, _)| format!("{}…", &line[..end]))
        }
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_capture_is_disabled_by_default() {
        assert!(!ClipboardSettings::default().enabled);
    }

    #[test]
    fn settings_json_without_new_fields_still_deserializes() {
        // 旧版持久化 JSON 中已移除的 blacklist 与缺失的新字段均不得导致设置整体重置。
        let old = r#"{"enabled":false,"capture_images":true,"max_item_bytes":1024,
            "blacklist":["com.example.app"],"auto_paste":false}"#;
        #[allow(clippy::unwrap_used)]
        let parsed: ClipboardSettings = serde_json::from_str::<ClipboardSettings>(old).unwrap();
        assert!(!parsed.enabled);
        assert!(!parsed.alternate_hotkey);
        #[allow(clippy::unwrap_used)]
        let serialized = serde_json::to_string(&parsed).unwrap();
        assert!(!serialized.contains("blacklist"));
    }

    #[test]
    fn settings_validation_bounds_resource_fields() {
        assert!(ClipboardSettings::default().validate().is_ok());

        let oversized_item = ClipboardSettings {
            max_item_bytes: MAX_CLIPBOARD_ITEM_BYTES + 1,
            ..ClipboardSettings::default()
        };
        assert!(oversized_item.validate().is_err());
    }

    #[test]
    fn fnv_hash_stable_known_values() {
        assert_eq!(fnv1a_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_hash(b"hello"), fnv1a_hash(b"hello"));
        assert_ne!(fnv1a_hash(b"hello"), fnv1a_hash(b"hellp"));
    }

    #[test]
    fn inline_payload_size_excludes_file_backed_image_bytes() {
        let now = Utc::now();
        let item = ClipItem {
            id: ClipId::new(),
            kind: ClipKind::Text,
            text: Some("abc".into()),
            rtf: Some(vec![0; 4]),
            image_path: Some("large.img".into()),
            thumb_path: None,
            image_dims: Some((10, 10)),
            files: vec!["/a".into(), "/bc".into()],
            preview: "preview".into(),
            source: None,
            byte_size: 1024 * 1024,
            content_hash: "hash".into(),
            created_at: now,
            last_used_at: now,
        };

        assert_eq!(item.inline_payload_bytes(), 12);
    }

    #[test]
    fn classify_url_color_text() {
        assert_eq!(classify_text("https://example.com/page"), ClipKind::Link);
        assert_eq!(classify_text("  http://a.cn/x  "), ClipKind::Link);
        assert_eq!(classify_text("https:// broken url"), ClipKind::Text);
        assert_eq!(classify_text("https://example.com\0hidden"), ClipKind::Text);
        assert_eq!(classify_text("#ff8800"), ClipKind::Color);
        assert_eq!(classify_text("#F80"), ClipKind::Color);
        assert_eq!(classify_text("#ff880042"), ClipKind::Color);
        assert_eq!(classify_text("#ggg"), ClipKind::Text);
        assert_eq!(classify_text("rgb(1, 2, 3)"), ClipKind::Color);
        assert_eq!(classify_text("hsla(0, 0%, 0%, 1)"), ClipKind::Color);
        assert_eq!(classify_text("SELECT 1;"), ClipKind::Text);
        assert!(!is_safe_http_url(&format!(
            "https://example.com/{}",
            "x".repeat(16 * 1024)
        )));
    }

    #[test]
    fn hex_color_parses() {
        assert_eq!(parse_hex_color("#ff8800"), Some((0xff, 0x88, 0x00)));
        assert_eq!(parse_hex_color("#f80"), Some((0xff, 0x88, 0x00)));
        assert_eq!(parse_hex_color("#ff880042"), Some((0xff, 0x88, 0x00)));
        assert_eq!(parse_hex_color("rgb(1,2,3)"), None);
    }

    #[test]
    fn preview_truncates_and_describes() {
        let long = "x".repeat(200);
        let p = make_preview(ClipKind::Text, Some(&long), &[], None);
        assert_eq!(p.chars().count(), 121);
        assert!(p.ends_with('…'));

        let multi = "first line\nsecond";
        assert_eq!(
            make_preview(ClipKind::Text, Some(multi), &[], None),
            "first line"
        );

        assert_eq!(
            make_preview(ClipKind::Image, None, &[], Some((800, 600))),
            "图片 800×600"
        );

        let files = vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()];
        assert_eq!(
            make_preview(ClipKind::Files, None, &files, None),
            "a.txt 等 2 个文件"
        );
        let one = vec!["/tmp/solo.png".to_string()];
        assert_eq!(make_preview(ClipKind::Files, None, &one, None), "solo.png");
    }
}
