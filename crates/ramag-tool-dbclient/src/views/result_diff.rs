//! 已加载查询结果的有界行列差异计算。

use std::{
    collections::hash_map::DefaultHasher,
    fmt::Write as _,
    hash::{Hash, Hasher},
    sync::Arc,
};

use ramag_domain::entities::{ConnectionConfig, ConnectionId, QueryResult};

/// 单次结果比较最多处理当前已加载的这些行，避免比较动作阻塞界面线程。
pub(crate) const MAX_COMPARE_ROWS: usize = 10_000;
const MAX_COLUMN_LINES: usize = 512;
const MAX_ROW_LINES: usize = 800;
pub(crate) const MAX_CELL_DIFFS: usize = 2_000;
const MAX_ROW_FIELDS: usize = 32;
const MAX_VALUE_PREVIEW_CHARS: usize = 120;
const MAX_ROW_PREVIEW_CHARS: usize = 4_096;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResultContext {
    pub(crate) connection_id: Option<ConnectionId>,
    pub(crate) default_schema: Option<String>,
    pub(crate) sql_hash: Option<u64>,
    pub(crate) pinned_target: Option<(Option<String>, String)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ResultScopeKey {
    pub(crate) page: Option<usize>,
    pub(crate) page_size: Option<usize>,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ResultSnapshot {
    pub(crate) result: Arc<QueryResult>,
    pub(crate) label: String,
    pub(crate) scope: String,
    pub(crate) context: ResultContext,
    pub(crate) scope_key: ResultScopeKey,
    pub(crate) identity_columns: Option<Vec<String>>,
}

impl ResultSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_query(
        result: Arc<QueryResult>,
        role: &str,
        connection: Option<&ConnectionConfig>,
        source_sql: Option<&str>,
        scope: String,
        scope_key: ResultScopeKey,
        pinned_target: Option<(Option<String>, String)>,
        identity_columns: Option<Vec<String>>,
        default_schema: Option<String>,
    ) -> Self {
        let query_label = source_sql
            .filter(|sql| !sql.trim().is_empty())
            .map(|sql| crate::views::inline_text_preview(sql, 96))
            .unwrap_or_else(|| "未命名查询".to_string());
        let connection_label = connection
            .map(|connection| crate::views::inline_text_preview(&connection.name, 64))
            .unwrap_or_else(|| "未连接".to_string());
        Self {
            result,
            label: format!("{role} · {connection_label} · SQL · {query_label}"),
            scope,
            context: ResultContext {
                connection_id: connection.map(|connection| connection.id.clone()),
                default_schema,
                sql_hash: source_sql.map(hash_text),
                pinned_target,
            },
            scope_key,
            identity_columns,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultDiffKind {
    Context,
    Added,
    Removed,
}

impl ResultDiffKind {
    pub(crate) fn prefix(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Added => '+',
            Self::Removed => '-',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResultDiffLine {
    pub(crate) kind: ResultDiffKind,
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResultCellDiff {
    pub(crate) source_row: usize,
    pub(crate) target_row: usize,
    pub(crate) source_column: usize,
    pub(crate) target_column: usize,
    pub(crate) column_name: String,
    pub(crate) source_value: String,
    pub(crate) target_value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowMatchMode {
    Identity,
    Content,
    Unavailable,
}

impl RowMatchMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Identity => "按主键或唯一键匹配",
            Self::Content => "按共有列内容匹配",
            Self::Unavailable => "无法匹配行",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResultDiff {
    pub(crate) column_lines: Vec<ResultDiffLine>,
    pub(crate) row_lines: Vec<ResultDiffLine>,
    pub(crate) cell_diffs: Vec<ResultCellDiff>,
    pub(crate) columns_added: usize,
    pub(crate) columns_removed: usize,
    pub(crate) columns_changed: usize,
    pub(crate) rows_changed: usize,
    pub(crate) rows_added: usize,
    pub(crate) rows_removed: usize,
    pub(crate) rows_unchanged: usize,
    pub(crate) source_rows: usize,
    pub(crate) target_rows: usize,
    pub(crate) source_rows_compared: usize,
    pub(crate) target_rows_compared: usize,
    pub(crate) unkeyed_source_rows: usize,
    pub(crate) unkeyed_target_rows: usize,
    pub(crate) omitted_column_lines: usize,
    pub(crate) omitted_row_lines: usize,
    pub(crate) omitted_cell_diffs: usize,
    pub(crate) row_mode: RowMatchMode,
    pub(crate) context_mismatch: bool,
    pub(crate) scope_mismatch: bool,
}

impl ResultDiff {
    pub(crate) fn has_changes(&self) -> bool {
        self.columns_added > 0
            || self.columns_removed > 0
            || self.columns_changed > 0
            || self.rows_changed > 0
            || self.rows_added > 0
            || self.rows_removed > 0
    }

    pub(crate) fn comparison_limited(&self) -> bool {
        self.source_rows_compared < self.source_rows
            || self.target_rows_compared < self.target_rows
            || self.unkeyed_source_rows > 0
            || self.unkeyed_target_rows > 0
            || self.omitted_cell_diffs > 0
    }
}

#[derive(Debug, Clone, Copy)]
struct ColumnPair {
    source: usize,
    target: usize,
}

#[derive(Debug, Default)]
struct ColumnStats {
    added: usize,
    removed: usize,
    changed: usize,
}

#[derive(Debug)]
struct RowComparison {
    lines: Vec<ResultDiffLine>,
    omitted_lines: usize,
    cell_diffs: Vec<ResultCellDiff>,
    omitted_cell_diffs: usize,
    source_rows_compared: usize,
    target_rows_compared: usize,
    added: usize,
    removed: usize,
    changed: usize,
    unchanged: usize,
    unkeyed_source: usize,
    unkeyed_target: usize,
    mode: RowMatchMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RowKey(Vec<u64>);

#[path = "result_diff_compute.rs"]
mod compute;
#[path = "result_diff_values.rs"]
mod values;

pub(crate) use compute::build_result_diff;

/// 将差异转换为可复制的纯文本，内容和界面中的颜色标记保持一致。
pub(crate) fn format_result_diff(
    source: &ResultSnapshot,
    target: &ResultSnapshot,
    diff: &ResultDiff,
) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "查询结果差异");
    let _ = writeln!(output, "基准：{}", source.label);
    let _ = writeln!(output, "基准范围：{}", source.scope);
    let _ = writeln!(output, "当前：{}", target.label);
    let _ = writeln!(output, "当前范围：{}", target.scope);
    let _ = writeln!(
        output,
        "字段：修改 {}，新增 {}，删除 {}；行：变更 {}，新增 {}，删除 {}，未变化 {}；单元格：变化 {}",
        diff.columns_changed,
        diff.columns_added,
        diff.columns_removed,
        diff.rows_changed,
        diff.rows_added,
        diff.rows_removed,
        diff.rows_unchanged,
        diff.cell_diffs
            .len()
            .saturating_add(diff.omitted_cell_diffs),
    );
    let _ = writeln!(output, "行匹配：{}", diff.row_mode.label());

    if diff.context_mismatch {
        let _ = writeln!(
            output,
            "警告：两次结果的连接、SQL 或表目标不同，差异只代表当前已加载内容"
        );
    }
    if diff.scope_mismatch {
        let _ = writeln!(
            output,
            "警告：两次结果的分页或截断范围不同，不能据此判断完整结果集差异"
        );
    }
    if diff.row_mode == RowMatchMode::Content {
        let _ = writeln!(
            output,
            "提示：未找到两侧都存在的稳定键；行修改可能显示为一条删除和一条新增"
        );
    } else if diff.row_mode == RowMatchMode::Unavailable {
        let _ = writeln!(output, "提示：两次结果没有共有列，因此未比较行内容");
    }
    if diff.comparison_limited() {
        if diff.source_rows_compared < diff.source_rows
            || diff.target_rows_compared < diff.target_rows
        {
            let _ = writeln!(
                output,
                "提示：每侧最多比较 {} 行，超出部分未参与差异计算",
                MAX_COMPARE_ROWS
            );
        }
        if diff.unkeyed_source_rows > 0 || diff.unkeyed_target_rows > 0 {
            let _ = writeln!(
                output,
                "提示：{} 个基准行、{} 个当前行缺少可用键值，按未匹配行处理",
                diff.unkeyed_source_rows, diff.unkeyed_target_rows
            );
        }
        if diff.omitted_cell_diffs > 0 {
            let _ = writeln!(
                output,
                "提示：单元格变化超过 {} 处，已省略 {} 处",
                MAX_CELL_DIFFS, diff.omitted_cell_diffs
            );
        }
    }

    append_lines(
        &mut output,
        "字段变化",
        &diff.column_lines,
        diff.omitted_column_lines,
    );
    append_lines(
        &mut output,
        "行变化",
        &diff.row_lines,
        diff.omitted_row_lines,
    );
    if !diff.cell_diffs.is_empty() || diff.omitted_cell_diffs > 0 {
        append_cell_diffs(&mut output, &diff.cell_diffs, diff.omitted_cell_diffs);
    }
    if !diff.has_changes() {
        let _ = writeln!(output, "\n结果一致（在上述已加载范围内）");
    }
    output
}

fn append_lines(output: &mut String, title: &str, lines: &[ResultDiffLine], omitted_lines: usize) {
    let _ = writeln!(output, "\n{title}");
    if lines.is_empty() {
        let _ = writeln!(output, "（无差异）");
        return;
    }
    for line in lines {
        let _ = writeln!(output, "{} {}", line.kind.prefix(), line.text);
    }
    if omitted_lines > 0 {
        let _ = writeln!(output, "… 还有 {omitted_lines} 行未显示");
    }
}

fn append_cell_diffs(output: &mut String, diffs: &[ResultCellDiff], omitted_diffs: usize) {
    let _ = writeln!(output, "\n单元格变化");
    for diff in diffs {
        let _ = writeln!(
            output,
            "基准[第 {} 行，第 {} 列] -> 当前[第 {} 行，第 {} 列]；列：{}；旧值：{}；新值：{}",
            diff.source_row + 1,
            diff.source_column + 1,
            diff.target_row + 1,
            diff.target_column + 1,
            diff.column_name,
            diff.source_value,
            diff.target_value,
        );
    }
    if omitted_diffs > 0 {
        let _ = writeln!(
            output,
            "… 还有 {omitted_diffs} 个单元格变化未显示（上限 {MAX_CELL_DIFFS}）"
        );
    }
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ramag_domain::entities::{Row, Value};

    use super::*;

    fn snapshot(
        columns: &[&str],
        types: &[&str],
        rows: Vec<Vec<Value>>,
        identity: Option<&[&str]>,
    ) -> ResultSnapshot {
        ResultSnapshot {
            result: Arc::new(QueryResult {
                columns: columns.iter().map(|column| (*column).into()).collect(),
                column_types: types.iter().map(|data_type| (*data_type).into()).collect(),
                rows: rows.into_iter().map(|values| Row { values }).collect(),
                affected_rows: 0,
                elapsed_ms: 0,
                warnings: Vec::new(),
                truncated: false,
            }),
            label: "SELECT test".into(),
            scope: "已加载 2 行".into(),
            context: ResultContext::default(),
            scope_key: ResultScopeKey::default(),
            identity_columns: identity
                .map(|columns| columns.iter().map(|column| (*column).into()).collect()),
        }
    }

    #[test]
    fn keyed_comparison_reports_changed_added_and_removed_rows() {
        let source = snapshot(
            &["id", "name"],
            &["BIGINT", "TEXT"],
            vec![
                vec![Value::Int(1), Value::Text("old".into())],
                vec![Value::Int(2), Value::Text("gone".into())],
            ],
            Some(&["id"]),
        );
        let target = snapshot(
            &["id", "name"],
            &["BIGINT", "TEXT"],
            vec![
                vec![Value::Int(1), Value::Text("new".into())],
                vec![Value::Int(3), Value::Text("added".into())],
            ],
            Some(&["id"]),
        );

        let diff = build_result_diff(&source, &target);

        assert_eq!(diff.row_mode, RowMatchMode::Identity);
        assert_eq!(diff.rows_changed, 1);
        assert_eq!(diff.rows_removed, 1);
        assert_eq!(diff.rows_added, 1);
        assert_eq!(diff.rows_unchanged, 0);
        assert_eq!(diff.cell_diffs.len(), 1);
        let cell = &diff.cell_diffs[0];
        assert_eq!(cell.source_row, 0);
        assert_eq!(cell.target_row, 0);
        assert_eq!(cell.source_column, 1);
        assert_eq!(cell.target_column, 1);
        assert_eq!(cell.column_name, "name");
        assert_eq!(cell.source_value, "old");
        assert_eq!(cell.target_value, "new");
        assert!(diff.row_lines.iter().any(|line| line.text.contains("old")));
        assert!(
            diff.row_lines
                .iter()
                .any(|line| line.text.contains("added"))
        );
        let copied = format_result_diff(&source, &target, &diff);
        assert!(copied.contains("基准[第 1 行，第 2 列] -> 当前[第 1 行，第 2 列]"));
        assert!(copied.contains("旧值：old；新值：new"));
    }

    #[test]
    fn column_comparison_reports_type_addition_and_removal() {
        let source = snapshot(
            &["id", "name"],
            &["INT", "TEXT"],
            vec![vec![Value::Int(1), Value::Text("same".into())]],
            None,
        );
        let target = snapshot(
            &["id", "email"],
            &["BIGINT", "TEXT"],
            vec![vec![Value::Int(1), Value::Text("same".into())]],
            None,
        );

        let diff = build_result_diff(&source, &target);

        assert_eq!(diff.columns_changed, 1);
        assert_eq!(diff.columns_removed, 1);
        assert_eq!(diff.columns_added, 1);
        assert!(
            diff.column_lines
                .iter()
                .any(|line| line.text.contains("name"))
        );
        assert!(
            diff.column_lines
                .iter()
                .any(|line| line.text.contains("email"))
        );
    }

    #[test]
    fn content_matching_is_order_independent_and_preserves_duplicates() {
        let source = snapshot(
            &["value"],
            &["TEXT"],
            vec![
                vec![Value::Text("same".into())],
                vec![Value::Text("same".into())],
            ],
            None,
        );
        let target = snapshot(
            &["value"],
            &["TEXT"],
            vec![
                vec![Value::Text("same".into())],
                vec![Value::Text("new".into())],
            ],
            None,
        );

        let diff = build_result_diff(&source, &target);

        assert_eq!(diff.row_mode, RowMatchMode::Content);
        assert_eq!(diff.rows_unchanged, 1);
        assert_eq!(diff.rows_removed, 1);
        assert_eq!(diff.rows_added, 1);
        assert_eq!(diff.rows_changed, 0);
        assert!(diff.cell_diffs.is_empty());
        assert_eq!(diff.omitted_cell_diffs, 0);
    }

    #[test]
    fn no_common_columns_skips_row_comparison() {
        let source = snapshot(&["id"], &["INT"], vec![vec![Value::Int(1)]], Some(&["id"]));
        let target = snapshot(
            &["code"],
            &["TEXT"],
            vec![vec![Value::Text("1".into())]],
            Some(&["code"]),
        );

        let diff = build_result_diff(&source, &target);

        assert_eq!(diff.row_mode, RowMatchMode::Unavailable);
        assert_eq!(diff.source_rows_compared, 0);
        assert_eq!(diff.target_rows_compared, 0);
        assert!(diff.row_lines.is_empty());
        assert!(diff.cell_diffs.is_empty());
    }

    #[test]
    fn comparison_is_bounded_to_loaded_rows() {
        let rows = (0..=MAX_COMPARE_ROWS)
            .map(|index| vec![Value::Int(index as i64)])
            .collect();
        let source = snapshot(&["id"], &["INT"], rows, None);
        let target = snapshot(&["id"], &["INT"], Vec::new(), None);

        let diff = build_result_diff(&source, &target);

        assert_eq!(diff.source_rows, MAX_COMPARE_ROWS + 1);
        assert_eq!(diff.source_rows_compared, MAX_COMPARE_ROWS);
        assert!(diff.comparison_limited());
    }

    #[test]
    fn cell_comparison_is_bounded_to_changed_cells() {
        let mut columns = vec!["id".to_string()];
        columns.extend((1..=(MAX_CELL_DIFFS + 1)).map(|index| format!("column_{index}")));
        let column_names: Vec<&str> = columns.iter().map(String::as_str).collect();
        let types: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(index, _)| if index == 0 { "BIGINT" } else { "TEXT" }.into())
            .collect();
        let type_names: Vec<&str> = types.iter().map(String::as_str).collect();
        let source_values: Vec<Value> = (0..columns.len())
            .map(|index| {
                if index == 0 {
                    Value::Int(1)
                } else {
                    Value::Text("old".into())
                }
            })
            .collect();
        let target_values: Vec<Value> = (0..columns.len())
            .map(|index| {
                if index == 0 {
                    Value::Int(1)
                } else {
                    Value::Text("new".into())
                }
            })
            .collect();
        let source = snapshot(
            &column_names,
            &type_names,
            vec![source_values],
            Some(&["id"]),
        );
        let target = snapshot(
            &column_names,
            &type_names,
            vec![target_values],
            Some(&["id"]),
        );

        let diff = build_result_diff(&source, &target);

        assert_eq!(diff.cell_diffs.len(), MAX_CELL_DIFFS);
        assert_eq!(diff.omitted_cell_diffs, 1);
        assert!(diff.comparison_limited());
        let copied = format_result_diff(&source, &target, &diff);
        assert!(copied.contains("单元格变化超过 2000 处，已省略 1 处"));
    }
}
