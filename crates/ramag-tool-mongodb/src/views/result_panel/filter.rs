//! 结果区「过滤列 / 过滤行」解析：列/行子串匹配（纯函数，可独立测试）

use super::flatten::FlatTable;

/// 过滤列框解析结果
pub(crate) struct ParsedFilter {
    /// 列过滤（小写、子串匹配）
    pub(crate) filters: Vec<String>,
}

/// 逗号分隔的列名 / dotted path 均只做过滤。嵌套下钻由单元格双击独立触发，
/// 避免同一个输入框因字段类型不同而改变导航状态。
pub(crate) fn classify_filter(raw: &str) -> ParsedFilter {
    ParsedFilter {
        filters: raw
            .split(',')
            .map(|token| token.trim().to_ascii_lowercase())
            .filter(|token| !token.is_empty())
            .collect(),
    }
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
            let lower = c.path.to_ascii_lowercase();
            filters.iter().any(|f| lower.contains(f))
        })
        .map(|(i, _)| i)
        .collect();
    Some(indices)
}

/// 行过滤：任意单元格子串包含 query（大小写不敏感）→ 行索引；空 query 返回 None（全显示）
pub(crate) fn row_indices_for(table: &FlatTable, query: &str) -> Option<Vec<usize>> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return None;
    }
    let indices: Vec<usize> = table
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.iter().any(|c| c.text.to_ascii_lowercase().contains(&q)))
        .map(|(i, _)| i)
        .collect();
    Some(indices)
}

#[cfg(test)]
mod tests {
    use super::super::flatten::{Column, FlatTable};
    use super::{classify_filter, column_indices_for};

    #[test]
    fn all_tokens_are_column_filters() {
        let p = classify_filter("project, project.items, appId");
        assert_eq!(p.filters, vec!["project", "project.items", "appid"]);
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
}
