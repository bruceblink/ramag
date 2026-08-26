use std::collections::HashMap;

use ramag_domain::entities::QueryResult;

use super::values::{
    content_key, format_column, format_row, identity_key, identity_values_equal, push_line,
    push_row_pair, rows_equal,
};
use super::{
    ColumnPair, ColumnStats, MAX_COLUMN_LINES, MAX_COMPARE_ROWS, MAX_ROW_LINES, ResultDiff,
    ResultDiffKind, ResultDiffLine, ResultSnapshot, RowComparison, RowKey, RowMatchMode,
};

/// 比较两次查询结果；只读取快照中的已加载行，不重新访问数据库。
pub(crate) fn build_result_diff(source: &ResultSnapshot, target: &ResultSnapshot) -> ResultDiff {
    let (column_lines, common_columns, column_stats, omitted_column_lines) =
        compare_columns(&source.result, &target.result);
    let row_comparison = if common_columns.is_empty() {
        RowComparison {
            lines: Vec::new(),
            omitted_lines: 0,
            source_rows_compared: 0,
            target_rows_compared: 0,
            added: 0,
            removed: 0,
            changed: 0,
            unchanged: 0,
            unkeyed_source: 0,
            unkeyed_target: 0,
            mode: RowMatchMode::Unavailable,
        }
    } else {
        let identity_columns = matching_identity_columns(source, target);
        compare_rows(
            &source.result,
            &target.result,
            &common_columns,
            identity_columns.as_deref(),
        )
    };

    ResultDiff {
        column_lines,
        row_lines: row_comparison.lines,
        columns_added: column_stats.added,
        columns_removed: column_stats.removed,
        columns_changed: column_stats.changed,
        rows_changed: row_comparison.changed,
        rows_added: row_comparison.added,
        rows_removed: row_comparison.removed,
        rows_unchanged: row_comparison.unchanged,
        source_rows: source.result.rows.len(),
        target_rows: target.result.rows.len(),
        source_rows_compared: row_comparison.source_rows_compared,
        target_rows_compared: row_comparison.target_rows_compared,
        unkeyed_source_rows: row_comparison.unkeyed_source,
        unkeyed_target_rows: row_comparison.unkeyed_target,
        omitted_column_lines,
        omitted_row_lines: row_comparison.omitted_lines,
        row_mode: row_comparison.mode,
        context_mismatch: source.context != target.context,
        scope_mismatch: source.scope_key != target.scope_key,
    }
}

fn compare_columns(
    source: &QueryResult,
    target: &QueryResult,
) -> (Vec<ResultDiffLine>, Vec<ColumnPair>, ColumnStats, usize) {
    let mut lines = Vec::new();
    let mut common_columns = Vec::new();
    let mut stats = ColumnStats::default();
    let mut omitted_lines = 0;
    let mut target_used = vec![false; target.columns.len()];

    for (source_index, source_name) in source.columns.iter().enumerate() {
        let target_index = target
            .columns
            .iter()
            .enumerate()
            .find_map(|(index, target_name)| {
                (!target_used[index] && source_name.eq_ignore_ascii_case(target_name))
                    .then_some(index)
            });
        let source_text = format_column(source, source_index);
        match target_index {
            Some(target_index) => {
                target_used[target_index] = true;
                common_columns.push(ColumnPair {
                    source: source_index,
                    target: target_index,
                });
                let target_text = format_column(target, target_index);
                if source_text == target_text {
                    push_line(
                        &mut lines,
                        MAX_COLUMN_LINES,
                        &mut omitted_lines,
                        ResultDiffKind::Context,
                        source_text,
                    );
                } else {
                    stats.changed += 1;
                    push_line(
                        &mut lines,
                        MAX_COLUMN_LINES,
                        &mut omitted_lines,
                        ResultDiffKind::Removed,
                        source_text,
                    );
                    push_line(
                        &mut lines,
                        MAX_COLUMN_LINES,
                        &mut omitted_lines,
                        ResultDiffKind::Added,
                        target_text,
                    );
                }
            }
            None => {
                stats.removed += 1;
                push_line(
                    &mut lines,
                    MAX_COLUMN_LINES,
                    &mut omitted_lines,
                    ResultDiffKind::Removed,
                    source_text,
                );
            }
        }
    }

    for (target_index, used) in target_used.into_iter().enumerate() {
        if !used {
            stats.added += 1;
            push_line(
                &mut lines,
                MAX_COLUMN_LINES,
                &mut omitted_lines,
                ResultDiffKind::Added,
                format_column(target, target_index),
            );
        }
    }

    (lines, common_columns, stats, omitted_lines)
}

fn matching_identity_columns(
    source: &ResultSnapshot,
    target: &ResultSnapshot,
) -> Option<Vec<ColumnPair>> {
    let source_identity = source.identity_columns.as_deref()?;
    let target_identity = target.identity_columns.as_deref()?;
    if source_identity.len() != target_identity.len()
        || source_identity
            .iter()
            .zip(target_identity)
            .any(|(source, target)| !source.eq_ignore_ascii_case(target))
    {
        return None;
    }

    let mut pairs = Vec::with_capacity(source_identity.len());
    for name in source_identity {
        let source_index = unique_column_index(&source.result, name)?;
        let target_index = unique_column_index(&target.result, name)?;
        pairs.push(ColumnPair {
            source: source_index,
            target: target_index,
        });
    }
    Some(pairs)
}

fn unique_column_index(result: &QueryResult, name: &str) -> Option<usize> {
    let mut matches = result
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.eq_ignore_ascii_case(name))
        .map(|(index, _)| index);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn compare_rows(
    source: &QueryResult,
    target: &QueryResult,
    common_columns: &[ColumnPair],
    identity_columns: Option<&[ColumnPair]>,
) -> RowComparison {
    let source_count = source.rows.len().min(MAX_COMPARE_ROWS);
    let target_count = target.rows.len().min(MAX_COMPARE_ROWS);
    match identity_columns {
        Some(identity_columns) if !identity_columns.is_empty() => compare_keyed_rows(
            source,
            target,
            common_columns,
            identity_columns,
            source_count,
            target_count,
        ),
        _ => compare_content_rows(source, target, common_columns, source_count, target_count),
    }
}

fn compare_keyed_rows(
    source: &QueryResult,
    target: &QueryResult,
    common_columns: &[ColumnPair],
    identity_columns: &[ColumnPair],
    source_count: usize,
    target_count: usize,
) -> RowComparison {
    let mut source_buckets: HashMap<RowKey, Vec<usize>> = HashMap::new();
    let mut source_keys = Vec::with_capacity(source_count);
    let mut unkeyed_source = 0;
    for index in 0..source_count {
        let key = identity_key(source.rows.get(index), identity_columns, true);
        if let Some(key) = &key {
            source_buckets.entry(key.clone()).or_default().push(index);
        } else {
            unkeyed_source += 1;
        }
        source_keys.push(key);
    }

    let mut target_for_source = vec![None; source_count];
    let mut target_matched = vec![false; target_count];
    let mut unkeyed_target = 0;
    for target_index in 0..target_count {
        let key = identity_key(target.rows.get(target_index), identity_columns, false);
        let Some(key) = key else {
            unkeyed_target += 1;
            continue;
        };
        let Some(source_candidates) = source_buckets.get(&key) else {
            continue;
        };
        let source_index = source_candidates.iter().copied().find(|source_index| {
            target_for_source[*source_index].is_none()
                && source_keys[*source_index].is_some()
                && identity_values_equal(
                    source.rows.get(*source_index),
                    target.rows.get(target_index),
                    identity_columns,
                )
        });
        if let Some(source_index) = source_index {
            target_for_source[source_index] = Some(target_index);
            target_matched[target_index] = true;
        }
    }

    let mut lines = Vec::new();
    let mut added = 0;
    let mut removed = 0;
    let mut changed = 0;
    let mut unchanged = 0;
    let mut omitted_lines = 0;
    for source_index in 0..source_count {
        match target_for_source[source_index] {
            Some(target_index)
                if rows_equal(
                    source.rows.get(source_index),
                    target.rows.get(target_index),
                    common_columns,
                ) =>
            {
                unchanged += 1;
            }
            Some(target_index) => {
                changed += 1;
                push_row_pair(
                    &mut lines,
                    &mut omitted_lines,
                    source,
                    target,
                    source_index,
                    target_index,
                );
            }
            None => {
                removed += 1;
                push_line(
                    &mut lines,
                    MAX_ROW_LINES,
                    &mut omitted_lines,
                    ResultDiffKind::Removed,
                    format_row(source, source_index),
                );
            }
        }
    }
    for target_index in 0..target_count {
        if !target_matched[target_index] {
            added += 1;
            push_line(
                &mut lines,
                MAX_ROW_LINES,
                &mut omitted_lines,
                ResultDiffKind::Added,
                format_row(target, target_index),
            );
        }
    }

    RowComparison {
        lines,
        omitted_lines,
        source_rows_compared: source_count,
        target_rows_compared: target_count,
        added,
        removed,
        changed,
        unchanged,
        unkeyed_source,
        unkeyed_target,
        mode: RowMatchMode::Identity,
    }
}

fn compare_content_rows(
    source: &QueryResult,
    target: &QueryResult,
    common_columns: &[ColumnPair],
    source_count: usize,
    target_count: usize,
) -> RowComparison {
    let mut source_buckets: HashMap<RowKey, Vec<usize>> = HashMap::new();
    for index in 0..source_count {
        source_buckets
            .entry(content_key(source.rows.get(index), common_columns, true))
            .or_default()
            .push(index);
    }

    let mut source_matched = vec![false; source_count];
    let mut target_matched = vec![false; target_count];
    for target_index in 0..target_count {
        let key = content_key(target.rows.get(target_index), common_columns, false);
        let Some(source_candidates) = source_buckets.get(&key) else {
            continue;
        };
        let source_index = source_candidates.iter().copied().find(|source_index| {
            !source_matched[*source_index]
                && rows_equal(
                    source.rows.get(*source_index),
                    target.rows.get(target_index),
                    common_columns,
                )
        });
        if let Some(source_index) = source_index {
            source_matched[source_index] = true;
            target_matched[target_index] = true;
        }
    }

    let mut lines = Vec::new();
    let mut added = 0;
    let mut removed = 0;
    let mut unchanged = 0;
    let mut omitted_lines = 0;
    for (index, matched) in source_matched.into_iter().enumerate() {
        if matched {
            unchanged += 1;
        } else {
            removed += 1;
            push_line(
                &mut lines,
                MAX_ROW_LINES,
                &mut omitted_lines,
                ResultDiffKind::Removed,
                format_row(source, index),
            );
        }
    }
    for (index, matched) in target_matched.into_iter().enumerate() {
        if !matched {
            added += 1;
            push_line(
                &mut lines,
                MAX_ROW_LINES,
                &mut omitted_lines,
                ResultDiffKind::Added,
                format_row(target, index),
            );
        }
    }

    RowComparison {
        lines,
        omitted_lines,
        source_rows_compared: source_count,
        target_rows_compared: target_count,
        added,
        removed,
        changed: 0,
        unchanged,
        unkeyed_source: 0,
        unkeyed_target: 0,
        mode: RowMatchMode::Content,
    }
}
