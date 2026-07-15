//! SQL 补全：实现 gpui-component CompletionProvider。
//! 覆盖关键字 / 表名 / 列名 / 点号限定（`表.列`、`库.表`）补全

use std::collections::{HashMap, HashSet};
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
use ropey::Rope;

/// Schema 元数据缓存：表 / 视图 / 列 / schema 列表，供补全与视图判定共用。
/// 把这个 Arc 传给 SqlCompletionProvider；后续即使重建编辑器也读同一份。
#[derive(Default)]
pub struct SchemaCache {
    /// schema → 表名列表
    pub tables: HashMap<String, Vec<String>>,
    /// schema → 视图名集合（包括普通视图 / 物化视图 / SYSTEM VIEW）
    /// 由 TableTreePanel 在 list_tables 时按 Table::is_view 提取写入；
    /// result_panel 用它判断当前查询的目标表是否视图，从而禁用写操作按钮
    pub views: HashMap<String, std::collections::HashSet<String>>,
    /// (schema, table) → 列名列表
    /// 由 QueryTab 编辑器变化时按 FROM/JOIN 用到的表预拉；列名补全 + 点号限定补全读取
    pub columns: HashMap<(String, String), Vec<String>>,
    /// 正在拉取列结构的表；避免连续输入或多个 Tab 对同一表重复发请求。
    pub loading_columns: HashSet<(String, String)>,
    /// 默认 schema（连接配置里的 database 字段）
    pub default_schema: Option<String>,
    /// 当前连接已知的所有 schema 名（不论是否展开）
    /// 由 TableTreePanel 在 list_schemas 成功后写入；DB 下拉读取
    pub all_schemas: Vec<String>,
    /// 表树侧"显示系统库"toggle 的当前状态
    /// 默认 false（隐藏）；DB 下拉读取此值决定是否展示系统库
    pub show_system: bool,
}

impl SchemaCache {
    /// 判断 schema.table 是否视图（不区分大小写匹配）。schema 为 None 时不判断，返回 false
    pub fn is_view(&self, schema: Option<&str>, table: &str) -> bool {
        let Some(s) = schema else {
            return false;
        };
        match self.views.get(s) {
            Some(set) => set.iter().any(|v| v.eq_ignore_ascii_case(table)),
            None => false,
        }
    }
}

impl SchemaCache {
    pub fn new_shared() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::default()))
    }

    /// 取所有可补全的表名（默认 schema 优先，其余次之）
    pub fn all_tables(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(d) = &self.default_schema
            && let Some(ts) = self.tables.get(d)
        {
            out.extend(ts.iter().cloned());
        }
        for (s, ts) in &self.tables {
            if Some(s) != self.default_schema.as_ref() {
                out.extend(ts.iter().cloned());
            }
        }
        out
    }
}

/// SQL 上下文：根据光标前的最后一个关键字猜测应补全什么
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlContext {
    /// 应补表名：FROM / JOIN / INTO / UPDATE / TABLE 后
    Table,
    /// 应补列名：SELECT 后（FROM 之前）/ WHERE / AND / OR / ON / HAVING / SET /
    /// ORDER BY / GROUP BY 后
    Column,
    /// 其他位置：仅补关键字
    Other,
}

/// 通过 cursor 前的纯大写文本，找最近的关键字判定上下文
fn detect_context(before_cursor: &str) -> SqlContext {
    let mut tokens = before_cursor.split_ascii_whitespace().rev().peekable();
    // 倒着扫，碰到第一个能定上下文的 token 就返回；不再复制整段 token 列表。
    while let Some(token) = tokens.next() {
        let token = token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
        if token.is_empty() {
            continue;
        }
        // 多词关键字：BY 前面是 ORDER / GROUP → 列名上下文。
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

/// 公开版本：让 QueryTab 编辑器变化时可以预拉这些表的列结构
/// 返回 (schema_可选, table) 对，schema 来自 `db.table` 这种全限定形式
pub fn extract_tables_in_use_for_prefetch(sql: &str) -> Vec<(Option<String>, String)> {
    extract_tables_with_schema(sql)
}

/// 从 SQL 中提取 FROM / JOIN / UPDATE / INTO 后的表名（仅名字版本）
/// 用于列名补全的查表名匹配（跨 schema）
fn extract_tables_in_use(sql: &str) -> Vec<String> {
    extract_tables_with_schema(sql)
        .into_iter()
        .map(|(_, t)| t)
        .collect()
}

/// 提取 (schema, table) 对：schema 来自全限定 `schema.table` 形式
/// 若是裸表名（无 schema 前缀），返回 (None, table)
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

/// `documentation` 走 markdown，长名在右侧 docs 面板可见（上游 CompletionMenu 行为）
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

/// SQL 补全 provider：关键字 + 表名（基于 cache）
pub struct SqlCompletionProvider {
    cache: Arc<RwLock<SchemaCache>>,
}

impl SqlCompletionProvider {
    pub fn new_rc(cache: Arc<RwLock<SchemaCache>>) -> Rc<dyn CompletionProvider> {
        Rc::new(Self { cache })
    }

    /// 点号限定补全：qualifier 命中别名/表名 → 补该表的列；命中库名 → 补该库的表
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
        // 1) qualifier 命中别名或表名 → 补该表的列
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
                // ref 带库名时要求库匹配，避免同名表跨库串列
                if let Some(rs) = &tref.schema
                    && !rs.eq_ignore_ascii_case(schema)
                {
                    continue;
                }
                for col in cols {
                    if col.to_ascii_lowercase().starts_with(prefix_lower) {
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
        // 2) qualifier 是库名 → 补该库的表（`mydb.` → 表名）
        for (s, ts) in cache.tables.iter() {
            if !s.eq_ignore_ascii_case(qualifier) {
                continue;
            }
            for t in ts {
                if t.to_ascii_lowercase().starts_with(prefix_lower) {
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

        // 取光标前的"单词"作为补全前缀（点号场景下即点号后的 partial）
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

        // 点号限定：partial 前若紧跟 `限定符.`，取出限定符（别名 / 表名 / 库名）走专门补全
        // 例：`u.na`→u；`users.`→users；`mydb.`→mydb。命中即返回，不掺关键字噪音
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

        // 非点号且前缀为空 → 没有可补的
        if prefix.is_empty() {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }

        let prefix_upper = prefix.to_ascii_uppercase();

        // 上下文判定：取前缀单词之前的全部文本（不含当前正在敲的）
        let before = &text[..start];
        let context = detect_context(before);

        let mut items: Vec<CompletionItem> = Vec::new();

        match context {
            // Table：建议表名（默认 schema 优先）；documentation 走 markdown 让长名在 docs 面板可见
            SqlContext::Table => {
                let cache = self.cache.read();
                let default_schema = cache.default_schema.clone();
                // 默认 schema 的表先入队，其他 schema 在后；保留 schema 上下文
                let mut order: Vec<(&String, &String)> = Vec::new();
                if let Some(d) = default_schema.as_ref()
                    && let Some(ts) = cache.tables.get(d)
                {
                    for t in ts {
                        order.push((d, t));
                    }
                }
                for (s, ts) in cache.tables.iter() {
                    if Some(s) == default_schema.as_ref() {
                        continue;
                    }
                    for t in ts {
                        order.push((s, t));
                    }
                }
                for (schema, name) in order {
                    if name.to_ascii_lowercase().starts_with(&prefix_lower) {
                        // 不用反引号 inline code（上游 markdown 渲染会染成饱和蓝块）；
                        // 用粗体名字 + 普通文本归属，配色更柔和
                        let doc = format!(
                            "**{name}**\n\nTable · schema **{schema}**{default_marker}",
                            default_marker = if Some(schema) == default_schema.as_ref() {
                                "（默认库）"
                            } else {
                                ""
                            }
                        );
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
            // Column：用整段 SQL 解析（FROM 可能在光标后，如 `SELECT t|<cursor> FROM users`）
            SqlContext::Column => {
                let tables_in_use = extract_tables_in_use(&text);
                let cache = self.cache.read();
                let mut seen = std::collections::HashSet::new();
                for table_name in &tables_in_use {
                    for ((schema, t), cols) in cache.columns.iter() {
                        if !t.eq_ignore_ascii_case(table_name) {
                            continue;
                        }
                        for col in cols {
                            if !seen.insert(col.clone()) {
                                continue;
                            }
                            if col.to_ascii_lowercase().starts_with(&prefix_lower) {
                                // 同表名补全：避免反引号 inline code 染成蓝块
                                let doc = format!("**{col}**\n\nColumn · in **{schema}.{t}**");
                                items.push(make_item(
                                    col.clone(),
                                    CompletionItemKind::FIELD,
                                    Some("column"),
                                    Some(doc),
                                    replace_range,
                                ));
                                if items.len() >= 30 {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            SqlContext::Other => {}
        }

        // 多词关键字短语前缀：从光标回退到最近的 SQL 分隔符，
        // 让"已敲完第一个词、正敲第二个词"的输入（如 "DROP T"）也能补出 "DROP TABLE"
        // —— 此时单词 prefix 只剩 "T"，匹配不到带空格的整短语
        let phrase = phrase_prefix(&text, real_offset);
        let phrase_upper = phrase.to_ascii_uppercase();
        let phrase_replace_range = lsp_types::Range::new(
            rope.offset_to_position(window_start_byte + real_offset - phrase.len()),
            end_pos,
        );

        // 关键字兜底，总数 ≤ 50
        for kw in SQL_KEYWORDS {
            if items.len() >= 50 {
                break;
            }
            if kw.starts_with(&prefix_upper) {
                // 单词前缀：替换当前词
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
                // 多词关键字第二个词起：用整段短语匹配，替换整个短语
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
        // 字母 / 数字 / 下划线 + 点号（点号触发 `表.列` 限定补全）
        new_text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}

/// 候选列 = ResultPanel 的当前结果列名，按光标前最近的 `,` 切 token 仅匹配最后一段
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
        let real_offset = offset.min(bytes.len());

        // 找当前 token 起点：从光标向前扫到最近的逗号（或文本起点）
        let mut tok_start = real_offset;
        while tok_start > 0 && bytes[tok_start - 1] != b',' {
            tok_start -= 1;
        }
        // 跳过前导空格
        while tok_start < real_offset && bytes[tok_start] == b' ' {
            tok_start += 1;
        }
        let prefix = &text[tok_start..real_offset];
        if prefix.is_empty() {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }
        let prefix_lower = prefix.to_ascii_lowercase();

        let start_pos = rope.offset_to_position(tok_start);
        let end_pos = rope.offset_to_position(real_offset);
        let replace_range = lsp_types::Range::new(start_pos, end_pos);

        // 已经填进过滤框的列（其它 token）不再建议，避免重复
        let already: std::collections::HashSet<String> = text
            .split(',')
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty() && *s != prefix_lower)
            .collect();

        let cols = self.columns.read();
        let mut items: Vec<CompletionItem> = Vec::new();
        for name in cols.iter() {
            let lc = name.to_ascii_lowercase();
            // 子串匹配（与表格过滤逻辑一致：大小写不敏感 contains）
            if !lc.contains(&prefix_lower) {
                continue;
            }
            if already.contains(&lc) {
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
        // 字母 / 数字 / 下划线触发；逗号不触发（逗号后用户还要输入下一个 token）
        new_text.chars().all(|c| c.is_alphanumeric() || c == '_')
    }
}

mod alias;
mod keywords;
pub use keywords::{SQL_KEYWORDS, SYSTEM_SCHEMAS, is_system_schema};
#[cfg(test)]
mod tests;
