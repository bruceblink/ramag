use std::sync::Arc;

use ramag_domain::entities::{QueryResult, Value};
use serde_json::Value as JsonValue;

use super::super::{PlanRow, PlanSource, PlanTree};
use super::{MAX_PLAN_ROWS, bounded_text, push_plan_row};

pub(super) fn parse_mysql_json_plan(result: &QueryResult) -> Option<PlanTree> {
    let json_column = column_index(&result.columns, "EXPLAIN")?;
    let mut rows = Vec::new();
    let mut truncated = false;
    for row in &result.rows {
        let Some(value) = row.values.get(json_column).and_then(json_value) else {
            continue;
        };
        let Some(query_block) = value.get("query_block") else {
            continue;
        };
        append_mysql_json_node(query_block, None, 0, &mut rows, &mut truncated);
    }
    (!rows.is_empty()).then(|| PlanTree {
        source: PlanSource::Mysql,
        rows: Arc::new(rows),
        truncated,
    })
}

pub(super) fn parse_postgres_json_plan(result: &QueryResult) -> Option<PlanTree> {
    let json_column = column_index(&result.columns, "QUERY PLAN")?;
    let mut rows = Vec::new();
    let mut truncated = false;
    for row in &result.rows {
        let Some(value) = row.values.get(json_column).and_then(json_value) else {
            continue;
        };
        let root = value
            .as_array()
            .and_then(|values| values.first())
            .unwrap_or(&value);
        let Some(plan) = root.get("Plan") else {
            continue;
        };
        let Some(root_id) = append_postgres_json_node(plan, None, 0, &mut rows, &mut truncated)
        else {
            continue;
        };
        for key in ["Planning Time", "Execution Time"] {
            if let Some(value) = root.get(key).and_then(json_scalar_text) {
                push_json_detail(&mut rows, Some(root_id), 1, key, value, &mut truncated);
            }
        }
    }
    (!rows.is_empty()).then(|| PlanTree {
        source: PlanSource::Postgres,
        rows: Arc::new(rows),
        truncated,
    })
}

// 将 MySQL JSON 执行计划递归展开为受限的可渲染节点，并保留截断状态。
fn append_mysql_json_node(
    value: &JsonValue,
    parent: Option<usize>,
    depth: usize,
    rows: &mut Vec<PlanRow>,
    truncated: &mut bool,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    if let Some(table) = object.get("table") {
        let Some(table_object) = table.as_object() else {
            return;
        };
        let Some(node_id) = push_json_node(
            rows,
            parent,
            depth,
            mysql_table_label(table_object),
            mysql_table_detail(table_object),
            truncated,
        ) else {
            return;
        };
        append_mysql_json_children(table_object, Some(node_id), depth + 1, rows, truncated);
        return;
    }
    if let Some((operation, operation_value)) = object
        .iter()
        .find(|(key, _)| MYSQL_OPERATION_KEYS.contains(&key.as_str()))
    {
        let operation = operation.as_str();
        let Some(node_id) = push_json_node(
            rows,
            parent,
            depth,
            operation.replace('_', " "),
            json_object_detail(object, &[operation]),
            truncated,
        ) else {
            return;
        };
        append_mysql_json_children(
            operation_value.as_object().unwrap_or(object),
            Some(node_id),
            depth + 1,
            rows,
            truncated,
        );
        return;
    }
    if let Some(nested_loop) = object.get("nested_loop").and_then(JsonValue::as_array) {
        let Some(node_id) = push_json_node(
            rows,
            parent,
            depth,
            "Nested Loop".to_string(),
            None,
            truncated,
        ) else {
            return;
        };
        for child in nested_loop {
            append_mysql_json_node(child, Some(node_id), depth + 1, rows, truncated);
        }
        return;
    }

    let Some(node_id) = push_json_node(
        rows,
        parent,
        depth,
        mysql_query_block_label(object),
        json_object_detail(object, &["nested_loop"]),
        truncated,
    ) else {
        return;
    };
    append_mysql_json_children(object, Some(node_id), depth + 1, rows, truncated);
}

fn append_mysql_json_children(
    object: &serde_json::Map<String, JsonValue>,
    parent: Option<usize>,
    depth: usize,
    rows: &mut Vec<PlanRow>,
    truncated: &mut bool,
) {
    if let Some(nested_loop) = object.get("nested_loop").and_then(JsonValue::as_array) {
        let Some(node_id) = push_json_node(
            rows,
            parent,
            depth,
            "Nested Loop".to_string(),
            None,
            truncated,
        ) else {
            return;
        };
        for child in nested_loop {
            append_mysql_json_node(child, Some(node_id), depth + 1, rows, truncated);
        }
    }
    for key in MYSQL_OPERATION_KEYS {
        let Some(value) = object.get(*key) else {
            continue;
        };
        let wrapper = serde_json::json!({ *key: value });
        append_mysql_json_node(&wrapper, parent, depth, rows, truncated);
    }
    if object.contains_key("table") {
        append_mysql_json_node(
            &JsonValue::Object(object.clone()),
            parent,
            depth,
            rows,
            truncated,
        );
    }
}

fn append_postgres_json_node(
    value: &JsonValue,
    parent: Option<usize>,
    depth: usize,
    rows: &mut Vec<PlanRow>,
    truncated: &mut bool,
) -> Option<usize> {
    let object = value.as_object()?;
    let node_id = push_json_node(
        rows,
        parent,
        depth,
        postgres_node_label(object),
        postgres_node_detail(object),
        truncated,
    )?;
    if let Some(children) = object.get("Plans").and_then(JsonValue::as_array) {
        for child in children {
            append_postgres_json_node(child, Some(node_id), depth + 1, rows, truncated);
        }
    }
    Some(node_id)
}

fn push_json_node(
    rows: &mut Vec<PlanRow>,
    parent: Option<usize>,
    depth: usize,
    label: String,
    detail: Option<String>,
    truncated: &mut bool,
) -> Option<usize> {
    if rows.len() >= MAX_PLAN_ROWS {
        *truncated = true;
        return None;
    }
    Some(push_plan_row(rows, parent, depth, label, detail, false))
}

fn push_json_detail(
    rows: &mut Vec<PlanRow>,
    parent: Option<usize>,
    depth: usize,
    label: &str,
    detail: String,
    truncated: &mut bool,
) {
    if rows.len() >= MAX_PLAN_ROWS {
        *truncated = true;
        return;
    }
    push_plan_row(rows, parent, depth, label.to_string(), Some(detail), true);
}

const MYSQL_OPERATION_KEYS: &[&str] = &[
    "grouping_operation",
    "ordering_operation",
    "duplicates_removal",
    "union_result",
    "windowing",
    "buffer_result",
];

fn mysql_query_block_label(object: &serde_json::Map<String, JsonValue>) -> String {
    object
        .get("select_id")
        .and_then(json_scalar_text)
        .map_or_else(
            || "Query Block".to_string(),
            |id| format!("Query Block · select_id={id}"),
        )
}

fn mysql_table_label(object: &serde_json::Map<String, JsonValue>) -> String {
    let table = object
        .get("table_name")
        .and_then(json_scalar_text)
        .unwrap_or_else(|| "Table".to_string());
    object
        .get("access_type")
        .and_then(json_scalar_text)
        .map_or(table.clone(), |access| format!("{table} · {access}"))
}

fn mysql_table_detail(object: &serde_json::Map<String, JsonValue>) -> Option<String> {
    json_object_detail(object, &["table_name", "access_type"])
}

fn json_object_detail(
    object: &serde_json::Map<String, JsonValue>,
    ignored: &[&str],
) -> Option<String> {
    let mut parts = Vec::new();
    for (key, value) in object {
        if ignored.contains(&key.as_str()) || key == "cost_info" {
            continue;
        }
        if let Some(value) = json_scalar_text(value) {
            parts.push(format!("{key}={value}"));
        }
    }
    if let Some(cost_info) = object.get("cost_info").and_then(JsonValue::as_object) {
        for (key, value) in cost_info {
            if let Some(value) = json_scalar_text(value) {
                parts.push(format!("cost_info.{key}={value}"));
            }
        }
    }
    (!parts.is_empty()).then(|| bounded_text(&parts.join(" · ")))
}

fn postgres_node_label(object: &serde_json::Map<String, JsonValue>) -> String {
    let mut parts = Vec::new();
    for key in ["Node Type", "Join Type", "Relation Name", "Index Name"] {
        if let Some(value) = object.get(key).and_then(json_scalar_text) {
            parts.push(value);
        }
    }
    if parts.is_empty() {
        "Plan Node".to_string()
    } else {
        parts.join(" · ")
    }
}

fn postgres_node_detail(object: &serde_json::Map<String, JsonValue>) -> Option<String> {
    let keys = [
        "Startup Cost",
        "Total Cost",
        "Plan Rows",
        "Plan Width",
        "Actual Startup Time",
        "Actual Total Time",
        "Actual Rows",
        "Actual Loops",
        "Rows Removed by Filter",
        "Rows Removed by Join Filter",
        "Filter",
        "Index Cond",
        "Join Filter",
        "Hash Cond",
        "Recheck Cond",
    ];
    let parts = keys
        .iter()
        .filter_map(|key| {
            object
                .get(*key)
                .and_then(json_scalar_text)
                .map(|value| format!("{key}={value}"))
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| bounded_text(&parts.join(" · ")))
}

fn json_value(value: &Value) -> Option<JsonValue> {
    match value {
        Value::Json(value) => Some(value.clone()),
        Value::Text(value) => serde_json::from_str(value).ok(),
        _ => None,
    }
}

fn json_scalar_text(value: &JsonValue) -> Option<String> {
    let text = match value {
        JsonValue::Null => return None,
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => value.clone(),
        JsonValue::Array(_) | JsonValue::Object(_) => serde_json::to_string(value).ok()?,
    };
    (!text.trim().is_empty()).then(|| bounded_text(&text))
}

fn column_index(columns: &[String], wanted: &str) -> Option<usize> {
    columns
        .iter()
        .position(|column| column.trim().eq_ignore_ascii_case(wanted))
}
