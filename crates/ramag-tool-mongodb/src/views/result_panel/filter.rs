//! 结果区「过滤列 / 过滤行」解析：钻取路径识别 + 列/行子串匹配（纯函数，可独立测试）

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::cell::extjson_cell;
use super::flatten::FlatTable;
use super::row_search::RowFilter;
use ramag_domain::entities::contains_case_insensitive;

/// 过滤列框解析结果
pub(crate) struct ParsedFilter {
    /// 钻取路径（object/array 字段或嵌套路径）→ 钻进去只看其内容（裸字段）
    pub(crate) drill_path: Option<String>,
    /// 列过滤（小写、子串匹配）：分号后投影字段，或标量列名
    pub(crate) filters: Vec<String>,
}

/// 解析过滤列框：`钻取路径 ; 投影字段`，按字段类型自动分派。
/// - 路径指向 object/array → 钻进去只看其字段（裸名、不保留其它列）；标量 → 当列过滤（保留其它列）
/// - 分号后字段 = 钻取层只显示这些列；无分号 = 钻取层全部字段
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
            // 嵌套路径钻取（project.items），保留原大小写供取值
            drill_path = Some(t.to_string());
        } else {
            match field_kind(docs, &t.to_ascii_lowercase()) {
                // object / array → 钻进去看里面
                Some(("object" | "array", real)) => drill_path = Some(real),
                // 标量 / 未知字段 → 当普通列过滤（保留"输入列名过滤"旧行为）
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

/// 顶层字段类型（大小写不敏感，取首个有值的文档）；返回 (kind, 原字段名)
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
                Value::Null => break, // 此文档该字段为空，看下一个文档
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

/// 按 filters 子串匹配列 path（大小写不敏感）→ 列索引；空 filters 返回 None（全显示），
/// 有过滤但无命中返回空向量（明确显示 0 列）。
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

/// 行过滤：普通模式做子串匹配，ID 模式按目标 BSON 类型和值精确匹配。
#[cfg(test)]
pub(crate) fn row_indices_for(table: &FlatTable, filter: &RowFilter) -> Option<Vec<usize>> {
    row_indices_for_cancellable(table, filter, None)
        .ok()
        .flatten()
}

/// 可取消行过滤；每批行检查一次，旧筛选任务无需继续扫描整张矩阵。
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
        // project 是对象 → 钻取；无投影 → filters 空（看全部字段）
        let p = classify_filter("project", &sample());
        assert_eq!(p.drill_path.as_deref(), Some("project"));
        assert!(p.filters.is_empty());
    }

    #[test]
    fn array_name_drills() {
        // geoms 是数组 → 钻取
        let p = classify_filter("geoms", &sample());
        assert_eq!(p.drill_path.as_deref(), Some("geoms"));
    }

    #[test]
    fn scalar_name_filters() {
        // appId 是标量 → 当列过滤（不钻取，保留旧行为）
        let p = classify_filter("appId", &sample());
        assert!(p.drill_path.is_none());
        assert_eq!(p.filters, vec!["appid".to_string()]);
    }

    #[test]
    fn drill_with_projection() {
        // project ; id, name → 钻进 project，投影裸字段 id / name
        let p = classify_filter("project ; id, name", &sample());
        assert_eq!(p.drill_path.as_deref(), Some("project"));
        assert_eq!(p.filters, vec!["id".to_string(), "name".to_string()]);
    }

    #[test]
    fn nested_path_drills() {
        // project.items ; id → 钻到 project.items，投影 id
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
        // 空 filters → None（全显示）
        assert!(column_indices_for(&t, &[]).is_none());
        // "name" 命中 name 列
        let idx = column_indices_for(&t, &["name".to_string()]).unwrap();
        assert_eq!(idx, vec![3]);
        // 有输入但无命中必须明确为空，不能回退成全显示。
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
