//! SQL 关键字、表名与列名补全。

mod cache;
mod object_locator;

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use gpui::{Context, Task, Window};
use gpui_component::RopeExt;
use gpui_component::input::{CompletionProvider, InputState};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Documentation, InsertReplaceEdit, MarkupContent, MarkupKind,
};
use parking_lot::RwLock;
use ramag_domain::entities::contains_case_insensitive;
use ropey::Rope;

pub use cache::SchemaCache;
pub(crate) use object_locator::{parse_table_reference, table_reference_at_cursor};

/// 单次补全 / 预拉最多跟踪这些不同表，避免异常长 SQL 发起大量元数据请求。
const MAX_EXTRACTED_TABLE_REFERENCES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlContext {
    Table,
    Column,
    Other,
}

fn detect_context(before_cursor: &str) -> SqlContext {
    let mut tokens = before_cursor.split_ascii_whitespace().rev().peekable();
    while let Some(token) = tokens.next() {
        let token = token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
        if token.is_empty() {
            continue;
        }
        if token.eq_ignore_ascii_case("BY")
            && tokens.peek().is_some_and(|previous| {
                let previous =
                    previous.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                previous.eq_ignore_ascii_case("ORDER") || previous.eq_ignore_ascii_case("GROUP")
            })
        {
            return SqlContext::Column;
        }
        if ["FROM", "JOIN", "INTO", "UPDATE", "TABLE"]
            .iter()
            .any(|keyword| token.eq_ignore_ascii_case(keyword))
        {
            return SqlContext::Table;
        }
        if [
            "SELECT", "WHERE", "AND", "OR", "ON", "USING", "HAVING", "SET", "DISTINCT",
        ]
        .iter()
        .any(|keyword| token.eq_ignore_ascii_case(keyword))
        {
            return SqlContext::Column;
        }
    }
    SqlContext::Other
}

/// 多词关键字短语前缀：从 offset 回退到最近的 SQL 分隔符（; , ( ) 换行），去掉前导空格。
/// 让 "DROP T" 这类"第一个词已敲完、正敲第二个词"的输入能补出 "DROP TABLE"
fn phrase_prefix(text: &str, offset: usize) -> &str {
    let bytes = text.as_bytes();
    let off = offset.min(bytes.len());
    let mut s = off;
    while s > 0 {
        if matches!(bytes[s - 1], b';' | b',' | b'(' | b')' | b'\n') {
            break;
        }
        s -= 1;
    }
    text[s..off].trim_start()
}

fn starts_with_ascii_case_insensitive(value: &str, prefix_lower: &str) -> bool {
    value
        .as_bytes()
        .get(..prefix_lower.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix_lower.as_bytes()))
}

fn column_filter_matches(
    name: &str,
    prefix_lower: &str,
    already: &std::collections::HashSet<String>,
) -> bool {
    contains_case_insensitive(name, prefix_lower)
        && (already.is_empty() || !already.contains(&name.to_lowercase()))
}

pub fn extract_tables_in_use_for_prefetch(sql: &str) -> Vec<(Option<String>, String)> {
    extract_tables_with_schema(sql)
}

fn extract_tables_in_use(sql: &str) -> Vec<String> {
    extract_tables_with_schema(sql)
        .into_iter()
        .map(|(_, t)| t)
        .collect()
}

fn extract_tables_with_schema(sql: &str) -> Vec<(Option<String>, String)> {
    let mut tables = Vec::new();
    let mut seen = HashSet::new();
    let mut tokens = sql.split_ascii_whitespace();
    while let Some(token) = tokens.next() {
        let keyword = token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
        if !["FROM", "JOIN", "INTO", "UPDATE"]
            .iter()
            .any(|expected| keyword.eq_ignore_ascii_case(expected))
        {
            continue;
        }
        let Some(raw) = tokens.next() else {
            break;
        };
        // 数据库标识符通常只有几十字节；异常长 token 不值得为补全复制和清洗。
        if raw.len() > 4 * 1024 {
            continue;
        }
        let cleaned: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        let mut count = 0usize;
        let mut previous = None;
        let mut current = None;
        for part in cleaned.split('.').filter(|part| !part.is_empty()) {
            count += 1;
            previous = current;
            current = Some(part);
        }
        let table = match (count, previous, current) {
            (1, _, Some(table)) => (None, table.to_string()),
            (2.., Some(schema), Some(table)) => (Some(schema.to_string()), table.to_string()),
            _ => continue,
        };
        if seen.insert(table.clone()) {
            tables.push(table);
            if tables.len() >= MAX_EXTRACTED_TABLE_REFERENCES {
                break;
            }
        }
    }
    tables
}

/// 大脚本补全只分析光标附近最多 256 KiB，避免每次按键复制整份编辑器内容。
const MAX_COMPLETION_ANALYSIS_BYTES: usize = 256 * 1024;

fn completion_source_window(rope: &Rope, offset: usize) -> (String, usize, usize) {
    let total_bytes = rope.len();
    let cursor_byte = rope.floor_char_boundary(offset.min(total_bytes));
    if total_bytes <= MAX_COMPLETION_ANALYSIS_BYTES {
        return (rope.to_string(), cursor_byte, 0);
    }
    let half = MAX_COMPLETION_ANALYSIS_BYTES / 2;
    let (start_target, end_target) = if cursor_byte <= half {
        (0, MAX_COMPLETION_ANALYSIS_BYTES)
    } else if total_bytes.saturating_sub(cursor_byte) <= half {
        (total_bytes - MAX_COMPLETION_ANALYSIS_BYTES, total_bytes)
    } else {
        (
            cursor_byte - half,
            cursor_byte - half + MAX_COMPLETION_ANALYSIS_BYTES,
        )
    };
    let start_byte = rope.ceil_char_boundary(start_target);
    let end_byte = rope.floor_char_boundary(end_target);
    (
        rope.slice(start_byte..end_byte).to_string(),
        cursor_byte.saturating_sub(start_byte),
        start_byte,
    )
}

fn make_item(
    label: String,
    kind: CompletionItemKind,
    detail: Option<&str>,
    documentation: Option<String>,
    range: lsp_types::Range,
) -> CompletionItem {
    CompletionItem {
        label: label.clone(),
        kind: Some(kind),
        detail: detail.map(|s| s.to_string()),
        documentation: documentation.map(|md| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            })
        }),
        text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
            new_text: label,
            insert: range,
            replace: range,
        })),
        ..Default::default()
    }
}

pub struct SqlCompletionProvider {
    cache: Arc<RwLock<SchemaCache>>,
}

impl SqlCompletionProvider {
    pub fn new_rc(cache: Arc<RwLock<SchemaCache>>) -> Rc<dyn CompletionProvider> {
        Rc::new(Self { cache })
    }

    fn qualified_completions(
        &self,
        text: &str,
        qualifier: &str,
        prefix_lower: &str,
        replace_range: lsp_types::Range,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        let cache = self.cache.read();
        let refs = alias::extract_table_refs(text);
        let target = refs
            .iter()
            .find(|r| {
                r.alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(qualifier))
            })
            .or_else(|| {
                refs.iter()
                    .find(|r| r.table.eq_ignore_ascii_case(qualifier))
            });
        if let Some(tref) = target {
            for ((schema, t), cols) in cache.columns.iter() {
                if !t.eq_ignore_ascii_case(&tref.table) {
                    continue;
                }
                if let Some(rs) = &tref.schema
                    && !rs.eq_ignore_ascii_case(schema)
                {
                    continue;
                }
                for col in cols {
                    if starts_with_ascii_case_insensitive(col, prefix_lower) {
                        let doc = format!("**{col}**\n\nColumn · in **{schema}.{t}**");
                        items.push(make_item(
                            col.clone(),
                            CompletionItemKind::FIELD,
                            Some("column"),
                            Some(doc),
                            replace_range,
                        ));
                        if items.len() >= 50 {
                            return items;
                        }
                    }
                }
            }
            return items;
        }
        for (s, ts) in cache.tables.iter() {
            if !s.eq_ignore_ascii_case(qualifier) {
                continue;
            }
            for t in ts {
                if starts_with_ascii_case_insensitive(t, prefix_lower) {
                    let doc = format!("**{t}**\n\nTable · schema **{s}**");
                    items.push(make_item(
                        t.clone(),
                        CompletionItemKind::CLASS,
                        Some("table"),
                        Some(doc),
                        replace_range,
                    ));
                    if items.len() >= 50 {
                        return items;
                    }
                }
            }
        }
        items
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let (text, real_offset, window_start_byte) = completion_source_window(rope, offset);
        let bytes = text.as_bytes();

        let mut start = real_offset;
        while start > 0 {
            let b = bytes[start - 1];
            if b.is_ascii_alphanumeric() || b == b'_' {
                start -= 1;
            } else {
                break;
            }
        }
        let prefix = &text[start..real_offset];

        let end_pos = rope.offset_to_position(window_start_byte + real_offset);
        let replace_range =
            lsp_types::Range::new(rope.offset_to_position(window_start_byte + start), end_pos);
        let prefix_lower = prefix.to_ascii_lowercase();

        // 点号前可能是别名、表名或库名，命中后不混入关键字。
        if start > 0 && bytes[start - 1] == b'.' {
            let dot = start - 1;
            let mut qs = dot;
            while qs > 0 && (bytes[qs - 1].is_ascii_alphanumeric() || bytes[qs - 1] == b'_') {
                qs -= 1;
            }
            if qs < dot {
                let items =
                    self.qualified_completions(&text, &text[qs..dot], &prefix_lower, replace_range);
                return Task::ready(Ok(CompletionResponse::Array(items)));
            }
        }

        if prefix.is_empty() {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }

        let prefix_upper = prefix.to_ascii_uppercase();

        let before = &text[..start];
        let context = detect_context(before);

        let mut items: Vec<CompletionItem> = Vec::new();

        match context {
            SqlContext::Table => {
                let cache = self.cache.read();
                let default_schema = cache.default_schema.clone();
                if let Some(d) = default_schema.as_ref()
                    && let Some(ts) = cache.tables.get(d)
                {
                    for name in ts {
                        if starts_with_ascii_case_insensitive(name, &prefix_lower) {
                            let doc = format!("**{name}**\n\nTable · schema **{d}**（默认库）");
                            items.push(make_item(
                                name.clone(),
                                CompletionItemKind::CLASS,
                                Some("table"),
                                Some(doc),
                                replace_range,
                            ));
                            if items.len() >= 30 {
                                break;
                            }
                        }
                    }
                }
                if items.len() < 30 {
                    'schemas: for (schema, tables) in &cache.tables {
                        if Some(schema) == default_schema.as_ref() {
                            continue;
                        }
                        for name in tables {
                            if starts_with_ascii_case_insensitive(name, &prefix_lower) {
                                let doc = format!("**{name}**\n\nTable · schema **{schema}**");
                                items.push(make_item(
                                    name.clone(),
                                    CompletionItemKind::CLASS,
                                    Some("table"),
                                    Some(doc),
                                    replace_range,
                                ));
                                if items.len() >= 30 {
                                    break 'schemas;
                                }
                            }
                        }
                    }
                }
            }
            SqlContext::Column => {
                let tables_in_use = extract_tables_in_use(&text);
                let cache = self.cache.read();
                let mut seen = std::collections::HashSet::new();
                'tables: for table_name in &tables_in_use {
                    for ((schema, t), cols) in cache.columns.iter() {
                        if !t.eq_ignore_ascii_case(table_name) {
                            continue;
                        }
                        for col in cols {
                            if !starts_with_ascii_case_insensitive(col, &prefix_lower)
                                || !seen.insert(col.clone())
                            {
                                continue;
                            }
                            let doc = format!("**{col}**\n\nColumn · in **{schema}.{t}**");
                            items.push(make_item(
                                col.clone(),
                                CompletionItemKind::FIELD,
                                Some("column"),
                                Some(doc),
                                replace_range,
                            ));
                            if items.len() >= 30 {
                                break 'tables;
                            }
                        }
                    }
                }
            }
            SqlContext::Other => {}
        }

        // 多词关键字需用完整短语前缀匹配。
        let phrase = phrase_prefix(&text, real_offset);
        let phrase_upper = phrase.to_ascii_uppercase();
        let phrase_replace_range = lsp_types::Range::new(
            rope.offset_to_position(window_start_byte + real_offset - phrase.len()),
            end_pos,
        );

        for kw in SQL_KEYWORDS {
            if items.len() >= 50 {
                break;
            }
            if kw.starts_with(&prefix_upper) {
                items.push(make_item(
                    kw.to_string(),
                    CompletionItemKind::KEYWORD,
                    None,
                    None,
                    replace_range,
                ));
            } else if phrase_upper.contains(' ')
                && kw.len() > phrase_upper.len()
                && kw.starts_with(&phrase_upper)
            {
                items.push(make_item(
                    kw.to_string(),
                    CompletionItemKind::KEYWORD,
                    None,
                    None,
                    phrase_replace_range,
                ));
            }
        }

        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        _cx: &mut Context<InputState>,
    ) -> bool {
        new_text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}

pub struct ColumnFilterCompletionProvider {
    columns: Arc<RwLock<Vec<String>>>,
}

impl ColumnFilterCompletionProvider {
    pub fn new_rc(columns: Arc<RwLock<Vec<String>>>) -> Rc<dyn CompletionProvider> {
        Rc::new(Self { columns })
    }
}

impl CompletionProvider for ColumnFilterCompletionProvider {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let text = rope.to_string();
        let bytes = text.as_bytes();
        let real_offset = rope.floor_char_boundary(offset.min(bytes.len()));

        let mut tok_start = real_offset;
        while tok_start > 0 && bytes[tok_start - 1] != b',' {
            tok_start -= 1;
        }
        while tok_start < real_offset && bytes[tok_start].is_ascii_whitespace() {
            tok_start += 1;
        }
        let prefix = &text[tok_start..real_offset];
        if prefix.is_empty() {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }
        let prefix_lower = prefix.to_lowercase();

        let start_pos = rope.offset_to_position(tok_start);
        let end_pos = rope.offset_to_position(real_offset);
        let replace_range = lsp_types::Range::new(start_pos, end_pos);

        let already: std::collections::HashSet<String> = text
            .split(',')
            .map(|t| t.trim().to_lowercase())
            .filter(|s| !s.is_empty() && *s != prefix_lower)
            .collect();

        let cols = self.columns.read();
        let mut items: Vec<CompletionItem> = Vec::new();
        for name in cols.iter() {
            if !column_filter_matches(name, &prefix_lower, &already) {
                continue;
            }
            items.push(make_item(
                name.clone(),
                CompletionItemKind::FIELD,
                Some("column"),
                Some(format!("**{name}**\n\nColumn · 当前结果集列")),
                replace_range,
            ));
            if items.len() >= 50 {
                break;
            }
        }
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        _cx: &mut Context<InputState>,
    ) -> bool {
        new_text.chars().all(|c| c.is_alphanumeric() || c == '_')
    }
}

mod alias;
mod keywords;
pub use keywords::{SQL_KEYWORDS, SYSTEM_SCHEMAS, is_system_schema};
#[cfg(test)]
mod tests;
