//! 代码语法快照：文件加载时一次解析，滚动时只查询并绘制可见行。
//! Diff 与 Project Files 共用；不支持的扩展名退化为有界纯文本渲染。

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;

use gpui::{
    AnyElement, HighlightStyle, Hsla, IntoElement, ParentElement, SharedString, Styled, StyledText,
    div,
};
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use ramag_domain::entities::{DiffLineKind, FileDiff};
use ropey::Rope;

/// 文件路径 → tree-sitter 语言名（均为 gpui-component `tree-sitter-languages` 内置）。
///
/// 先按完整文件名匹配（Makefile / CMakeLists.txt 等无后缀），再按扩展名；
/// 都不在表内（Cargo.lock、.gitignore 等）→ None，调用方走纯文本渲染。
pub(super) fn lang_for_path(path: &str) -> Option<&'static str> {
    // 仅取最后一段文件名，避免目录名里的点干扰
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name {
        "Makefile" | "makefile" | "GNUmakefile" => return Some("make"),
        "CMakeLists.txt" => return Some("cmake"),
        _ => {}
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e)?;
    let lang = match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" => "python",
        "json" | "jsonc" => "json",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sql" => "sql",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" => "bash",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "scala" | "sbt" => "scala",
        "ex" | "exs" => "elixir",
        "cs" => "csharp",
        "html" | "htm" => "html",
        "css" => "css",
        "svelte" => "svelte",
        "astro" => "astro",
        "ejs" => "ejs",
        "erb" => "erb",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "zig" => "zig",
        "mk" => "make",
        "cmake" => "cmake",
        "diff" | "patch" => "diff",
        _ => return None,
    };
    Some(lang)
}

/// 制表位宽度：tab 展开到 4 列边界（与等宽渲染一致）
const TAB_W: usize = 4;
/// 超长行完整显示，但跳过语法高亮；与 gpui-component Code Editor 的保护一致。
pub(super) const MAX_HIGHLIGHT_LINE_BYTES: usize = 10_000;
/// 单份语法树最多解析 8 MiB；更大 Diff 仍可流畅查看，但退化为纯文本。
const MAX_HIGHLIGHT_SOURCE_BYTES: usize = 8 * 1024 * 1024;
/// 只缓存最近查询过的行，避免极端多行文件把 `Option<Vec<_>>` 预分配到数十 MiB。
const MAX_CACHED_HIGHLIGHT_LINES: usize = 8 * 1024;
/// Diff 同时保留旧、新两侧文本；超过此规模时直接按可见行渲染纯文本，避免首次打开等待整树解析。
const MAX_DIFF_SNAPSHOT_LINES: usize = 20_000;
const MAX_DIFF_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

/// 单字符显示列宽：CJK / 全角 / emoji ≈ 2 列，其余 1 列（近似，不引第三方 crate）
fn char_cols(c: char) -> usize {
    let cp = c as u32;
    if (0x1100..=0x115F).contains(&cp)
        || (0x2E80..=0xA4CF).contains(&cp)
        || (0xAC00..=0xD7A3).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFE30..=0xFE4F).contains(&cp)
        || (0xFF00..=0xFF60).contains(&cp)
        || (0xFFE0..=0xFFE6).contains(&cp)
        || (0x1F300..=0x1FAFF).contains(&cp)
        || (0x20000..=0x3FFFD).contains(&cp)
    {
        2
    } else {
        1
    }
}

/// 文本显示列数：与实际渲染使用同一份 Tab 展开规则。
pub(super) fn display_cols(text: &str) -> usize {
    text.chars().fold(0usize, |col, character| {
        if character == '\t' {
            col + TAB_W - (col % TAB_W)
        } else {
            col + char_cols(character)
        }
    })
}

#[derive(Debug)]
struct PreparedDisplayLine {
    text: String,
    /// 超长行不查询高亮，但正文必须完整显示。
    highlight_len: Option<usize>,
}

/// 只展开 Tab，不截断用户内容；极端长行仅关闭高亮。
fn prepare_display_line(text: &str) -> PreparedDisplayLine {
    let mut out = String::with_capacity(text.len());
    let mut col = 0usize;
    for c in text.chars() {
        if c == '\t' {
            let spaces = TAB_W - (col % TAB_W);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(c);
            col += char_cols(c);
        }
    }

    PreparedDisplayLine {
        highlight_len: (out.len() <= MAX_HIGHLIGHT_LINE_BYTES).then_some(out.len()),
        text: out,
    }
}

fn append_expanded_line(source: &mut String, text: &str) -> Option<Range<usize>> {
    let start = source.len();
    let mut col = 0usize;
    for c in text.chars() {
        if c == '\t' {
            let spaces = TAB_W - (col % TAB_W);
            if source.len().saturating_add(spaces).saturating_add(1) > MAX_HIGHLIGHT_SOURCE_BYTES {
                return None;
            }
            for _ in 0..spaces {
                source.push(' ');
            }
            col += spaces;
        } else {
            if source.len().saturating_add(c.len_utf8()).saturating_add(1)
                > MAX_HIGHLIGHT_SOURCE_BYTES
            {
                return None;
            }
            source.push(c);
            col += char_cols(c);
        }
    }
    let end = source.len();
    source.push('\n');
    Some(start..end)
}

#[derive(Debug)]
struct SyntaxLine {
    text: SharedString,
    source_range: Option<Range<usize>>,
}

struct LineStyleCache {
    theme_key: Option<u64>,
    styles: HashMap<usize, Vec<(Range<usize>, HighlightStyle)>>,
    order: VecDeque<usize>,
}

impl LineStyleCache {
    fn reset(&mut self, theme_key: u64) {
        self.theme_key = Some(theme_key);
        self.styles.clear();
        self.order.clear();
    }

    fn insert(&mut self, index: usize, styles: Vec<(Range<usize>, HighlightStyle)>) {
        while self.styles.len() >= MAX_CACHED_HIGHLIGHT_LINES {
            let Some(oldest) = self.order.pop_front() else {
                self.styles.clear();
                break;
            };
            self.styles.remove(&oldest);
        }
        self.order.push_back(index);
        self.styles.insert(index, styles);
    }
}

/// 只读文本的持久语法状态。生产数据应在线程池构造，渲染阶段不得调用构造函数。
pub(super) struct SyntaxDocument {
    lines: Vec<SyntaxLine>,
    highlighter: Option<SyntaxHighlighter>,
    style_cache: RefCell<LineStyleCache>,
    retained_bytes: usize,
}

impl SyntaxDocument {
    pub(super) fn new<'a>(lines: impl IntoIterator<Item = &'a str>, lang: Option<&str>) -> Self {
        let mut syntax_lines = Vec::new();
        let mut source = lang.map(|_| String::new());
        let mut display_bytes = 0usize;

        for text in lines {
            let display = prepare_display_line(text);
            display_bytes = display_bytes.saturating_add(display.text.len());

            let source_range = if let Some(source_text) = source.as_mut() {
                match append_expanded_line(source_text, text) {
                    Some(full_range) => display.highlight_len.map(|highlight_len| {
                        full_range.start
                            ..full_range
                                .start
                                .saturating_add(highlight_len)
                                .min(full_range.end)
                    }),
                    None => {
                        source = None;
                        None
                    }
                }
            } else {
                None
            };
            syntax_lines.push(SyntaxLine {
                text: SharedString::from(display.text),
                source_range,
            });
        }

        let highlighter = source.and_then(|source_text| {
            if source_text.is_empty() {
                return None;
            }
            let mut highlighter = SyntaxHighlighter::new(lang.unwrap_or("text"));
            highlighter.update(None, &Rope::from_str(&source_text), None);
            Some(highlighter)
        });
        if highlighter.is_none() {
            for line in &mut syntax_lines {
                line.source_range = None;
            }
        }

        let source_bytes = highlighter
            .as_ref()
            .map_or(0, |highlighter| highlighter.text().len());
        let retained_bytes = display_bytes.saturating_add(source_bytes).saturating_add(
            syntax_lines
                .capacity()
                .saturating_mul(std::mem::size_of::<SyntaxLine>()),
        );
        Self {
            lines: syntax_lines,
            highlighter,
            style_cache: RefCell::new(LineStyleCache {
                theme_key: None,
                styles: HashMap::new(),
                order: VecDeque::new(),
            }),
            retained_bytes,
        }
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(super) fn line(
        &self,
        index: usize,
        theme: &HighlightTheme,
        theme_key: u64,
    ) -> Option<CodeLine> {
        let line = self.lines.get(index)?;
        let highlights = self.line_styles(index, line, theme, theme_key);
        Some(CodeLine {
            text: line.text.clone(),
            highlights,
        })
    }

    fn line_styles(
        &self,
        index: usize,
        line: &SyntaxLine,
        theme: &HighlightTheme,
        theme_key: u64,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let Some(highlighter) = self.highlighter.as_ref() else {
            return Vec::new();
        };
        let Some(source_range) = line.source_range.as_ref() else {
            return Vec::new();
        };

        {
            let mut cache = self.style_cache.borrow_mut();
            if cache.theme_key != Some(theme_key) {
                cache.reset(theme_key);
            }
            if let Some(styles) = cache.styles.get(&index) {
                return styles.clone();
            }
        }

        let styles = highlighter
            .styles(source_range, theme)
            .into_iter()
            .filter_map(|(range, style)| {
                let start = range.start.max(source_range.start);
                let end = range.end.min(source_range.end);
                (start < end).then(|| {
                    (
                        start.saturating_sub(source_range.start)
                            ..end.saturating_sub(source_range.start),
                        style,
                    )
                })
            })
            .collect::<Vec<_>>();
        self.style_cache.borrow_mut().insert(index, styles.clone());
        styles
    }
}

#[derive(Clone, Copy, Default)]
struct DiffLineSyntax {
    old: Option<usize>,
    new: Option<usize>,
}

/// 一份 Diff 的旧侧、新侧持久语法树，以及 domain 行到两侧文档行的映射。
pub(super) struct DiffSyntaxSnapshot {
    old: SyntaxDocument,
    new: SyntaxDocument,
    line_map: Vec<Vec<DiffLineSyntax>>,
    retained_bytes: usize,
}

impl DiffSyntaxSnapshot {
    /// 只为有界 Diff 构建完整语法快照；超限时调用方以虚拟列表按需渲染纯文本。
    pub(super) fn new_bounded(diff: &FileDiff, lang: Option<&str>) -> Option<Self> {
        let mut side_lines = 0usize;
        let mut expanded_bytes = 0usize;
        for (hunk_index, hunk) in diff.hunks.iter().enumerate() {
            if hunk_index > 0 {
                side_lines = side_lines.checked_add(2)?;
                expanded_bytes = expanded_bytes.checked_add(2)?;
            }
            for line in &hunk.lines {
                let copies = if matches!(line.kind, DiffLineKind::Context) {
                    2
                } else {
                    1
                };
                side_lines = side_lines.checked_add(copies)?;
                expanded_bytes = expanded_bytes.checked_add(
                    expanded_line_bytes(&line.text)
                        .checked_add(1)?
                        .checked_mul(copies)?,
                )?;
                if side_lines > MAX_DIFF_SNAPSHOT_LINES || expanded_bytes > MAX_DIFF_SNAPSHOT_BYTES
                {
                    return None;
                }
            }
        }
        Some(Self::new(diff, lang))
    }

    pub(super) fn new(diff: &FileDiff, lang: Option<&str>) -> Self {
        let mut old_lines = Vec::new();
        let mut new_lines = Vec::new();
        let mut line_map = Vec::with_capacity(diff.hunks.len());

        for (hunk_index, hunk) in diff.hunks.iter().enumerate() {
            if hunk_index > 0 {
                old_lines.push("");
                new_lines.push("");
            }
            let mut hunk_map = Vec::with_capacity(hunk.lines.len());
            for line in &hunk.lines {
                let mut mapped = DiffLineSyntax::default();
                match line.kind {
                    DiffLineKind::Delete => {
                        mapped.old = Some(old_lines.len());
                        old_lines.push(line.text.as_str());
                    }
                    DiffLineKind::Add => {
                        mapped.new = Some(new_lines.len());
                        new_lines.push(line.text.as_str());
                    }
                    DiffLineKind::Context => {
                        mapped.old = Some(old_lines.len());
                        old_lines.push(line.text.as_str());
                        mapped.new = Some(new_lines.len());
                        new_lines.push(line.text.as_str());
                    }
                }
                hunk_map.push(mapped);
            }
            line_map.push(hunk_map);
        }

        let old = SyntaxDocument::new(old_lines, lang);
        let new = SyntaxDocument::new(new_lines, lang);
        let retained_bytes = old
            .retained_bytes()
            .saturating_add(new.retained_bytes())
            .saturating_add(
                line_map
                    .iter()
                    .map(|lines| {
                        lines
                            .capacity()
                            .saturating_mul(std::mem::size_of::<DiffLineSyntax>())
                    })
                    .sum::<usize>(),
            );
        Self {
            old,
            new,
            line_map,
            retained_bytes,
        }
    }

    pub(super) fn unified_line(
        &self,
        hunk_index: usize,
        line_index: usize,
        kind: DiffLineKind,
        theme: &HighlightTheme,
        theme_key: u64,
    ) -> Option<CodeLine> {
        match kind {
            DiffLineKind::Delete => self.side_line(hunk_index, line_index, true, theme, theme_key),
            DiffLineKind::Add | DiffLineKind::Context => {
                self.side_line(hunk_index, line_index, false, theme, theme_key)
            }
        }
    }

    pub(super) fn side_line(
        &self,
        hunk_index: usize,
        line_index: usize,
        old_side: bool,
        theme: &HighlightTheme,
        theme_key: u64,
    ) -> Option<CodeLine> {
        let mapped = self.line_map.get(hunk_index)?.get(line_index)?;
        if old_side {
            self.old.line(mapped.old?, theme, theme_key)
        } else {
            self.new.line(mapped.new?, theme, theme_key)
        }
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

fn expanded_line_bytes(text: &str) -> usize {
    let mut bytes = 0usize;
    let mut col = 0usize;
    for character in text.chars() {
        if character == '\t' {
            let spaces = TAB_W - (col % TAB_W);
            bytes = bytes.saturating_add(spaces);
            col = col.saturating_add(spaces);
        } else {
            bytes = bytes.saturating_add(character.len_utf8());
            col = col.saturating_add(char_cols(character));
        }
    }
    bytes
}

pub(super) struct CodeLine {
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
}

pub(super) fn plain_code_line(text: &str) -> CodeLine {
    CodeLine {
        text: SharedString::from(prepare_display_line(text).text),
        highlights: Vec::new(),
    }
}

pub(super) fn highlight_theme_key(theme: &HighlightTheme) -> u64 {
    let mut hasher = DefaultHasher::new();
    theme.hash(&mut hasher);
    hasher.finish()
}

/// 渲染阶段只创建一个 StyledText；语法解析和 Token UI 节点均不在滚动热路径。
pub(super) fn render_code_line(line: CodeLine, fg: Hsla, mono: SharedString) -> AnyElement {
    let text = if line.highlights.is_empty() {
        StyledText::new(line.text)
    } else {
        StyledText::new(line.text).with_highlights(line.highlights)
    };
    div()
        .text_xs()
        .font_family(mono)
        .text_color(fg)
        .whitespace_nowrap()
        .child(text)
        .into_any_element()
}

#[cfg(test)]
#[path = "syntax/tests.rs"]
mod tests;
