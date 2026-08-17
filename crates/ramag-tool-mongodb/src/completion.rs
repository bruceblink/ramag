//! MongoDB 命令与结果列补全。

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

/// 补全前缀回看窗口上限。
const MAX_COMMAND_COMPLETION_PREFIX_BYTES: usize = 4 * 1024;

/// 常用 runCommand 命令和参数。
const MONGO_COMMANDS: &[&str] = &[
    "find",
    "aggregate",
    "count",
    "distinct",
    "insert",
    "update",
    "delete",
    "findAndModify",
    "getMore",
    "listCollections",
    "listIndexes",
    "createIndexes",
    "dropIndexes",
    "drop",
    "create",
    "renameCollection",
    "ping",
    "dbStats",
    "collStats",
    "serverStatus",
    "filter",
    "projection",
    "sort",
    "limit",
    "skip",
    "pipeline",
    "query",
    "documents",
    "updates",
    "deletes",
    "cursor",
    "batchSize",
    "hint",
    "collation",
    "new",
    "upsert",
    "multi",
    "ordered",
];

/// 查询和聚合操作符。
const MONGO_OPERATORS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$and",
    "$or",
    "$not",
    "$nor",
    "$exists",
    "$type",
    "$regex",
    "$expr",
    "$mod",
    "$text",
    "$where",
    "$all",
    "$elemMatch",
    "$size",
    "$match",
    "$group",
    "$project",
    "$sort",
    "$limit",
    "$skip",
    "$unwind",
    "$lookup",
    "$count",
    "$facet",
    "$addFields",
    "$set",
    "$unset",
    "$sortByCount",
    "$sample",
    "$sum",
    "$avg",
    "$min",
    "$max",
    "$first",
    "$last",
    "$push",
    "$addToSet",
    "$concat",
    "$cond",
    "$ifNull",
    "$dateToString",
];

/// 创建可覆盖当前前缀的补全项。
fn make_item(
    label: &str,
    kind: CompletionItemKind,
    detail: &str,
    docs: String,
    range: lsp_types::Range,
) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        detail: Some(detail.to_string()),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: docs,
        })),
        text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
            new_text: label.to_string(),
            insert: range,
            replace: range,
        })),
        ..Default::default()
    }
}

/// 返回光标前补全前缀的起点和内容。
fn word_prefix(text: &str, offset: usize) -> (usize, &str) {
    let bytes = text.as_bytes();
    let end = offset.min(bytes.len());
    let mut start = end;
    while start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' {
            start -= 1;
        } else {
            break;
        }
    }
    (start, &text[start..end])
}

fn starts_with_ascii_case_insensitive(candidate: &str, prefix: &str) -> bool {
    candidate
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn normalized_path_segments(path: &str) -> Vec<String> {
    path.split('.').map(str::to_lowercase).collect()
}

/// 按路径段比较，避免 Unicode 大小写转换改变字节偏移。
fn child_after_path<'a>(name: &'a str, parent_lower: &[String]) -> Option<(&'a str, bool)> {
    let mut segments = name.split('.');
    for expected in parent_lower {
        let actual = segments.next()?;
        if actual.to_lowercase() != *expected {
            return None;
        }
    }
    let child = segments.next()?;
    Some((child, segments.next().is_some()))
}

fn path_prefix_segments(name: &str, count: usize) -> String {
    name.split('.').take(count).collect::<Vec<_>>().join(".")
}

/// 收集路径下的子字段及其可下钻状态。
fn dotted_child_candidates(
    cols: &[String],
    parent_lower: &[String],
    seg_prefix_lower: &str,
    limit: usize,
) -> Vec<(String, bool)> {
    let mut children: Vec<(String, bool)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for name in cols {
        let Some((seg, expandable)) = child_after_path(name, parent_lower) else {
            continue;
        };
        let seg_lower = seg.to_lowercase();
        if seg.is_empty() || !seg_lower.contains(seg_prefix_lower) {
            continue;
        }
        if let Some(&i) = index.get(&seg_lower) {
            // 任一路径更深即可下钻。
            children[i].1 = children[i].1 || expandable;
        } else if children.len() < limit {
            index.insert(seg_lower, children.len());
            children.push((
                path_prefix_segments(name, parent_lower.len() + 1),
                expandable,
            ));
        }
    }
    children
}

/// MongoDB 命令编辑器补全。
pub struct CommandCompletionProvider;

impl CommandCompletionProvider {
    pub fn new_rc() -> Rc<dyn CompletionProvider> {
        Rc::new(Self)
    }
}

impl CompletionProvider for CommandCompletionProvider {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let Some((start, real_offset, prefix)) = command_completion_prefix(rope, offset) else {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        };
        let range = lsp_types::Range::new(
            rope.offset_to_position(start),
            rope.offset_to_position(real_offset),
        );

        let mut items: Vec<CompletionItem> = Vec::new();
        if prefix.starts_with('$') {
            for op in MONGO_OPERATORS {
                if starts_with_ascii_case_insensitive(op, &prefix) {
                    items.push(make_item(
                        op,
                        CompletionItemKind::OPERATOR,
                        "operator",
                        format!("**{op}**\n\nOperator · MongoDB 查询 / 聚合操作符"),
                        range,
                    ));
                }
            }
        } else {
            for kw in MONGO_COMMANDS {
                if starts_with_ascii_case_insensitive(kw, &prefix) {
                    items.push(make_item(
                        kw,
                        CompletionItemKind::KEYWORD,
                        "command",
                        format!("**{kw}**\n\nCommand · MongoDB 数据库命令"),
                        range,
                    ));
                }
            }
        }
        items.truncate(50);
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
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    }
}

fn command_completion_prefix(rope: &Rope, offset: usize) -> Option<(usize, usize, String)> {
    let real_offset = rope.floor_char_boundary(offset.min(rope.len()));
    let window_start =
        rope.ceil_char_boundary(real_offset.saturating_sub(MAX_COMMAND_COMPLETION_PREFIX_BYTES));
    let text = rope.slice(window_start..real_offset).to_string();
    let (local_start, prefix) = word_prefix(&text, text.len());
    if prefix.is_empty() {
        return None;
    }

    // 避免截断超长标识符后只替换后半段。
    if local_start == 0 && window_start > 0 {
        let previous_start = rope.floor_char_boundary(window_start.saturating_sub(1));
        let previous = rope.slice(previous_start..window_start).to_string();
        if previous
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            return None;
        }
    }

    Some((
        window_start.saturating_add(local_start),
        real_offset,
        prefix.to_string(),
    ))
}

/// 结果列路径补全。
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
        // 从最近分隔符后的第一个非空白字符开始。
        let mut tok_start = real_offset;
        while tok_start > 0 && bytes[tok_start - 1] != b',' && bytes[tok_start - 1] != b';' {
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
        let range = lsp_types::Range::new(
            rope.offset_to_position(tok_start),
            rope.offset_to_position(real_offset),
        );

        // 不重复建议已填入的列。
        let already: std::collections::HashSet<String> = text
            .split([',', ';'])
            .map(|t| t.trim().to_lowercase())
            .filter(|s| !s.is_empty() && *s != prefix_lower)
            .collect();

        let cols = self.columns.read();
        let mut items: Vec<CompletionItem> = Vec::new();
        // 分号后仅补全钻取路径下的子字段。
        let drill = text[..real_offset]
            .rsplit_once(';')
            .map(|(head, _)| head.rsplit(',').next().unwrap_or("").trim())
            .filter(|d| !d.is_empty());
        if let Some(drill) = drill {
            let parent_lower = normalized_path_segments(drill);
            let mut seen = std::collections::HashSet::new();
            for name in cols.iter() {
                let Some((orig_seg, expandable)) = child_after_path(name, &parent_lower) else {
                    continue;
                };
                let seg_lc = orig_seg.to_lowercase();
                if seg_lc.is_empty()
                    || !seg_lc.contains(&prefix_lower)
                    || already.contains(&seg_lc)
                    || !seen.insert(seg_lc.clone())
                {
                    continue;
                }
                let (detail, docs) = if expandable {
                    (
                        "object",
                        format!("**{orig_seg}**\n\nObject · 嵌套字段，可打点继续下钻"),
                    )
                } else {
                    (
                        "field",
                        format!("**{orig_seg}**\n\nField · 叶子字段，钻取后为值列表"),
                    )
                };
                items.push(make_item(
                    orig_seg,
                    CompletionItemKind::FIELD,
                    detail,
                    docs,
                    range,
                ));
                if items.len() >= 50 {
                    break;
                }
            }
        } else if prefix.contains('.') {
            // 点号后深入补全子字段。
            let (parent, seg_prefix) = prefix.rsplit_once('.').unwrap_or(("", prefix));
            let parent_lower = normalized_path_segments(parent);
            for (full, expandable) in dotted_child_candidates(
                cols.as_slice(),
                &parent_lower,
                &seg_prefix.to_lowercase(),
                50,
            ) {
                let (detail, docs) = if expandable {
                    (
                        "object",
                        format!("**{full}**\n\nObject · 嵌套字段，可打点继续下钻"),
                    )
                } else {
                    (
                        "field",
                        format!("**{full}**\n\nField · 叶子字段，钻取后为值列表"),
                    )
                };
                items.push(make_item(
                    &full,
                    CompletionItemKind::FIELD,
                    detail,
                    docs,
                    range,
                ));
            }
        } else {
            // 无点时仅补全顶层字段。
            let mut seen = std::collections::HashSet::new();
            for name in cols.iter() {
                let top = name.split('.').next().unwrap_or(name);
                let lc = top.to_lowercase();
                if !lc.contains(&prefix_lower) || already.contains(&lc) || !seen.insert(lc.clone())
                {
                    continue;
                }
                items.push(make_item(
                    top,
                    CompletionItemKind::FIELD,
                    "column",
                    format!("**{top}**\n\nColumn · 当前结果集列"),
                    range,
                ));
                if items.len() >= 50 {
                    break;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_prefix_includes_dollar_and_stops_at_quote() {
        assert_eq!(word_prefix("{\"$gt", 5).1, "$gt");
        assert_eq!(word_prefix("\"find", 5).1, "find");
        assert_eq!(word_prefix("a, b", 1).1, "a");
        assert_eq!(word_prefix("{ ", 2).1, "");
    }

    #[test]
    fn command_completion_reads_only_the_cursor_window() {
        let head = "你".repeat(32 * 1024);
        let text = format!(r#"{head} {{"$gr"#);
        let rope = Rope::from_str(&text);
        let expected_start = text.len() - "$gr".len();

        assert_eq!(
            command_completion_prefix(&rope, rope.len()),
            Some((expected_start, text.len(), "$gr".to_string()))
        );
    }

    #[test]
    fn overlong_completion_token_is_not_partially_replaced() {
        let text = "a".repeat(MAX_COMMAND_COMPLETION_PREFIX_BYTES + 1);
        let rope = Rope::from_str(&text);

        assert!(command_completion_prefix(&rope, rope.len()).is_none());
    }

    #[test]
    fn unicode_path_matching_does_not_slice_by_lowercase_byte_length() {
        let parent = normalized_path_segments("İ");
        let (child, expandable) = child_after_path("İ.名称.deep", &parent).unwrap();

        assert_eq!(child, "名称");
        assert!(expandable);
        assert_eq!(path_prefix_segments("İ.名称.deep", 2), "İ.名称");
    }

    #[test]
    fn dotted_completion_includes_scalar_leaves_and_marks_drillable() {
        let cols = vec![
            "labels".to_string(),
            "labels.name".to_string(),
            "labels.meta".to_string(),
            "labels.meta.x".to_string(),
        ];
        let out = dotted_child_candidates(&cols, &["labels".to_string()], "", 50);
        let by_name = |n: &str| out.iter().find(|(f, _)| f == n).map(|(_, e)| *e);
        assert_eq!(by_name("labels.name"), Some(false));
        assert_eq!(by_name("labels.meta"), Some(true));
    }

    #[test]
    fn command_candidates_match_without_allocating_lowercase_copies() {
        assert!(starts_with_ascii_case_insensitive("findAndModify", "FIND"));
        assert!(starts_with_ascii_case_insensitive("$group", "$GR"));
        assert!(!starts_with_ascii_case_insensitive("find", "update"));
    }
}
