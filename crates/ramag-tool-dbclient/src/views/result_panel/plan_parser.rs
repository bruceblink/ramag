use std::sync::Arc;

use ramag_domain::entities::{QueryResult, Row, Value};

use super::{PlanRow, PlanSource, PlanTree};

#[path = "plan_json_parser.rs"]
mod json_parser;

const MAX_PLAN_ROWS: usize = 10_000;
const MAX_PLAN_TEXT_CHARS: usize = 16_384;

pub(super) fn parse_plan(result: &QueryResult) -> Option<PlanTree> {
    json_parser::parse_mysql_json_plan(result)
        .or_else(|| json_parser::parse_postgres_json_plan(result))
        .or_else(|| parse_mysql_plan(result))
        .or_else(|| parse_postgres_plan(result))
}

fn parse_mysql_plan(result: &QueryResult) -> Option<PlanTree> {
    let id_column = column_index(&result.columns, "id")?;
    let select_type = column_index(&result.columns, "select_type");
    let table = column_index(&result.columns, "table");
    let access_type = column_index(&result.columns, "type");
    let extra = column_index(&result.columns, "Extra");
    if select_type.is_none() && extra.is_none() {
        return None;
    }
    let detail_columns = [
        ("partitions", column_index(&result.columns, "partitions")),
        (
            "possible_keys",
            column_index(&result.columns, "possible_keys"),
        ),
        ("key", column_index(&result.columns, "key")),
        ("key_len", column_index(&result.columns, "key_len")),
        ("ref", column_index(&result.columns, "ref")),
        ("rows", column_index(&result.columns, "rows")),
        ("filtered", column_index(&result.columns, "filtered")),
        ("Extra", extra),
    ];
    let mut rows: Vec<PlanRow> = Vec::new();
    let mut stack: Vec<(u64, usize, usize)> = Vec::new();
    let mut truncated = false;
    for (row_index, row) in result.rows.iter().enumerate() {
        if rows.len() >= MAX_PLAN_ROWS {
            truncated = true;
            break;
        }
        let Some(id) = value_string(row, id_column).and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        while stack
            .last()
            .is_some_and(|(parent_id, _, _)| *parent_id >= id)
        {
            stack.pop();
        }
        let parent = stack.last().map(|(_, index, _)| *index);
        let depth = stack
            .last()
            .map(|(_, _, parent_depth)| parent_depth + 1)
            .unwrap_or(0);
        let mut label_parts = Vec::new();
        for index in [select_type, table, access_type].into_iter().flatten() {
            if let Some(value) = value_string(row, index) {
                label_parts.push(value);
            }
        }
        let label = if label_parts.is_empty() {
            format!("步骤 {}", row_index + 1)
        } else {
            label_parts.join(" · ")
        };
        let mut details = vec![format!("id={id}")];
        for (name, index) in detail_columns {
            if let Some(index) = index
                && let Some(value) = value_string(row, index)
            {
                details.push(format!("{name}={value}"));
            }
        }
        let node_id = push_plan_row(
            &mut rows,
            parent,
            depth,
            label,
            Some(details.join(" · ")),
            false,
        );
        stack.push((id, node_id, depth));
    }
    (!rows.is_empty()).then(|| PlanTree {
        source: PlanSource::Mysql,
        rows: Arc::new(rows),
        truncated,
    })
}

fn parse_postgres_plan(result: &QueryResult) -> Option<PlanTree> {
    let plan_column = result
        .columns
        .iter()
        .position(|column| column.trim().eq_ignore_ascii_case("QUERY PLAN"))?;
    let mut rows: Vec<PlanRow> = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut node_count = 0usize;
    let mut truncated = false;
    'result_rows: for row in &result.rows {
        let Some(value) = value_string(row, plan_column) else {
            continue;
        };
        for line in value.lines() {
            if rows.len() >= MAX_PLAN_ROWS {
                truncated = true;
                break 'result_rows;
            }
            let line = line.trim_end();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if is_plan_detail(trimmed) {
                let (label, detail) = split_plan_detail(trimmed);
                let parent = stack.last().map(|(_, index)| *index);
                let depth = stack
                    .last()
                    .and_then(|(_, index)| rows.get(*index))
                    .map(|parent| parent.depth + 1)
                    .unwrap_or(0);
                push_plan_row(&mut rows, parent, depth, label, detail, true);
                continue;
            }
            if !is_plan_node(trimmed) {
                continue;
            }
            let leading_spaces = line.len().saturating_sub(line.trim_start().len());
            let has_arrow = trimmed.starts_with("->");
            let node_label = trimmed
                .strip_prefix("->")
                .map(str::trim_start)
                .unwrap_or(trimmed)
                .to_string();
            let mut depth = leading_spaces / 2;
            if has_arrow && depth == 0 {
                depth = stack
                    .last()
                    .map(|(parent_depth, _)| parent_depth + 1)
                    .unwrap_or(0);
            }
            while stack
                .last()
                .is_some_and(|(parent_depth, _)| *parent_depth >= depth)
            {
                stack.pop();
            }
            let parent = stack.last().map(|(_, index)| *index);
            let node_id = push_plan_row(&mut rows, parent, depth, node_label, None, false);
            stack.push((depth, node_id));
            node_count = node_count.saturating_add(1);
        }
    }
    (node_count > 0).then(|| PlanTree {
        source: PlanSource::Postgres,
        rows: Arc::new(rows),
        truncated,
    })
}

fn push_plan_row(
    rows: &mut Vec<PlanRow>,
    parent: Option<usize>,
    depth: usize,
    label: String,
    detail: Option<String>,
    is_detail: bool,
) -> usize {
    let id = rows.len();
    rows.push(PlanRow {
        id,
        parent,
        depth,
        label,
        detail,
        is_detail,
        has_children: false,
    });
    if let Some(parent) = parent
        && let Some(parent_row) = rows.get_mut(parent)
    {
        parent_row.has_children = true;
    }
    id
}

fn column_index(columns: &[String], wanted: &str) -> Option<usize> {
    columns
        .iter()
        .position(|column| column.trim().eq_ignore_ascii_case(wanted))
}

fn value_string(row: &Row, index: usize) -> Option<String> {
    let value = row.values.get(index)?;
    let text = match value {
        Value::Null => return None,
        Value::Text(text) => text.clone(),
        Value::Json(value) => value.to_string(),
        other => other.to_clipboard_string(),
    };
    (!text.trim().is_empty()).then(|| bounded_text(&text))
}

fn bounded_text(text: &str) -> String {
    let mut chars = text.chars();
    let mut bounded = String::new();
    for _ in 0..MAX_PLAN_TEXT_CHARS {
        let Some(ch) = chars.next() else {
            return bounded;
        };
        bounded.push(ch);
    }
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn is_plan_detail(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "actual rows",
        "batches:",
        "execution time:",
        "filter:",
        "group key:",
        "hash cond:",
        "index cond:",
        "join filter:",
        "merge cond:",
        "memory usage:",
        "one-time filter:",
        "output:",
        "peak memory usage:",
        "planning time:",
        "presorted key:",
        "query identifier:",
        "recheck cond:",
        "rows removed by",
        "sort key:",
        "sort method:",
        "subplans removed:",
        "workers launched:",
        "workers planned:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn split_plan_detail(line: &str) -> (String, Option<String>) {
    line.split_once(':').map_or_else(
        || (line.to_string(), None),
        |(label, detail)| {
            let detail = detail.trim();
            (
                label.trim().to_string(),
                (!detail.is_empty()).then(|| bounded_text(detail)),
            )
        },
    )
}

fn is_plan_node(line: &str) -> bool {
    let candidate = line.strip_prefix("->").map(str::trim_start).unwrap_or(line);
    let lower = candidate.to_ascii_lowercase();
    lower.contains("(cost=")
        || [
            "aggregate",
            "append",
            "bitmap",
            "custom scan",
            "delete",
            "foreign scan",
            "gather",
            "hash",
            "incremental sort",
            "index only scan",
            "index scan",
            "join",
            "limit",
            "lockrows",
            "materialize",
            "memoize",
            "merge join",
            "modifytable",
            "nested loop",
            "parallel",
            "recursive union",
            "result",
            "scan",
            "seq scan",
            "setop",
            "sort",
            "subquery scan",
            "table function scan",
            "tid scan",
            "unique",
            "update",
            "values scan",
            "windowagg",
            "worktable scan",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}
