//! MongoDB 结果区的列/行过滤工具。

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::cell::extjson_cell;
use super::flatten::FlatTable;
use super::row_search::RowFilter;
use ramag_domain::entities::contains_case_insensitive;

/// 解析后的列筛选。
pub(crate) struct ParsedFilter {
    /// 要钻取的对象或数组路径。
    pub(crate) drill_path: Option<String>,
    /// 小写子串过滤词。
    pub(crate) filters: Vec<String>,
}

/// 解析“钻取路径;投影字段”筛选。
pub(crate) fn classify_filter(raw: &str, docs: &[Value]) -> ParsedFilter {
    let (head, tail) = raw.split_once(';').unwrap_or((raw, ""));
    let mut drill_path = None;
    let mut filters = Vec::new();
    for tok in head.split(',') {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        if t.contains('.') {
            drill_path = Some(t.to_string());
        } else {
            match field_kind(docs, &t.to_ascii_lowercase()) {
                Some(("object" | "array", real)) => drill_path = Some(real),
                _ => filters.push(t.to_ascii_lowercase()),
            }
        }
    }
    for f in tail.split(',') {
        let f = f.trim();
        if !f.is_empty() {
            filters.push(f.to_ascii_lowercase());
        }
    }
    ParsedFilter {
        drill_path,
        filters,
    }
}

/// 返回顶层字段类型及原始字段名。
fn field_kind(docs: &[Value], name_lower: &str) -> Option<(&'static str, String)> {
    for doc in docs {
        let Value::Object(map) = doc else {
            continue;
        };
        for (k, v) in map {
            if k.to_ascii_lowercase() != name_lower {
                continue;
            }
            match v {
                Value::Null => break, // 继续寻找非空值
                Value::Array(_) => return Some(("array", k.clone())),
                Value::Object(o) if extjson_cell(o).is_none() => {
                    return Some(("object", k.clone()));
                }
                _ => return Some(("scalar", k.clone())),
            }
        }
    }
    None
}

/// 返回匹配列索引；空筛选为 `None`，未命中为 `Some([])`。
pub(crate) fn column_indices_for(table: &FlatTable, filters: &[String]) -> Option<Vec<usize>> {
    if filters.is_empty() {
        return None;
    }
    let indices: Vec<usize> = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            filters
                .iter()
                .any(|filter| contains_case_insensitive(&c.path, filter))
        })
        .map(|(i, _)| i)
        .collect();
    Some(indices)
}

/// 行过滤：文本子串或 BSON ID 精确匹配。
#[cfg(test)]
pub(crate) fn row_indices_for(table: &FlatTable, filter: &RowFilter) -> Option<Vec<usize>> {
    row_indices_for_cancellable(table, filter, None)
        .ok()
        .flatten()
}

/// 可取消的行过滤。
pub(crate) fn row_indices_for_cancellable(
    table: &FlatTable,
    filter: &RowFilter,
    cancelled: Option<&AtomicBool>,
) -> Result<Option<Vec<usize>>, ()> {
    if !filter.is_active() {
        return Ok(None);
    }
    let mut indices = Vec::new();
    for (index, row) in table.rows.iter().enumerate() {
        if index % 64 == 0 && cancelled.is_some_and(|token| token.load(Ordering::Relaxed)) {
            return Err(());
        }
        if row.iter().any(|cell| filter.matches(cell)) {
            indices.push(index);
        }
    }
    Ok(Some(indices))
}

#[cfg(test)]
mod tests {
    use super::super::cell::Cell;
    use super::super::flatten::{Column, FlatTable};
    use super::{RowFilter, classify_filter, column_indices_for, row_indices_for};
    use serde_json::json;

    fn sample() -> Vec<serde_json::Value> {
        vec![json!({
            "_id": "x",
            "appId": "a",
            "geoms": [1, 2],
            "project": {"id": "p", "name": "n", "items": {"id": "i"}}
        })]
    }

    #[test]
    fn object_name_drills() {
        let p = classify_filter("project", &sample());
        assert_eq!(p.drill_path.as_deref(), Some("project"));
        assert!(p.filters.is_empty());
    }

    #[test]
    fn array_name_drills() {
        let p = classify_filter("geoms", &sample());
        assert_eq!(p.drill_path.as_deref(), Some("geoms"));
    }

    #[test]
    fn scalar_name_filters() {
        let p = classify_filter("appId", &sample());
        assert!(p.drill_path.is_none());
        assert_eq!(p.filters, vec!["appid".to_string()]);
    }

    #[test]
    fn drill_with_projection() {
        let p = classify_filter("project ; id, name", &sample());
        assert_eq!(p.drill_path.as_deref(), Some("project"));
        assert_eq!(p.filters, vec!["id".to_string(), "name".to_string()]);
    }

    #[test]
    fn nested_path_drills() {
        let p = classify_filter("project.items ; id", &sample());
        assert_eq!(p.drill_path.as_deref(), Some("project.items"));
        assert_eq!(p.filters, vec!["id".to_string()]);
    }

    fn table_of(cols: &[&str]) -> FlatTable {
        FlatTable {
            columns: cols
                .iter()
                .map(|c| Column {
                    path: c.to_string(),
                    kind: "text",
                })
                .collect(),
            rows: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn column_filter_substring_and_empty() {
        let t = table_of(&["_id", "consume.cost", "id", "name"]);
        assert!(column_indices_for(&t, &[]).is_none());
        let idx = column_indices_for(&t, &["name".to_string()]).unwrap();
        assert_eq!(idx, vec![3]);
        assert_eq!(
            column_indices_for(&t, &["missing".to_string()]),
            Some(vec![])
        );
    }

    #[test]
    fn row_filter_is_case_insensitive_for_ascii_and_unicode() {
        let table = FlatTable {
            columns: vec![Column {
                path: "name".into(),
                kind: "text",
            }],
            total_columns: 1,
            rows: vec![
                vec![Cell {
                    text: "前缀 Hello 世界".into(),
                    kind: "text",
                }],
                vec![Cell {
                    text: "ÜBER".into(),
                    kind: "text",
                }],
            ],
        };

        assert_eq!(
            row_indices_for(&table, &RowFilter::Text("hello".into())),
            Some(vec![0])
        );
        assert_eq!(
            row_indices_for(&table, &RowFilter::Text("über".into())),
            Some(vec![1])
        );
        assert!(row_indices_for(&table, &RowFilter::Text(String::new())).is_none());
    }
}
