//! MongoDB 文档按首层字段展平，嵌套值保留摘要并支持按需展开。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::cell::{Cell, cell_for_value, extjson_cell};

const MAX_TABLE_COLUMNS: usize = 512;
const MAX_TABLE_CELLS: usize = 2_000_000;
const MAX_COMPLETION_PATHS: usize = 10_000;

#[derive(Debug, Clone)]
pub struct Column {
    pub path: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct FlatTable {
    pub columns: Vec<Column>,
    /// 大于 `columns.len()` 表示矩阵已按预算裁剪。
    pub total_columns: usize,
    pub rows: Vec<Vec<Cell>>,
}

impl FlatTable {
    /// 估算表格派生视图的常驻内存。
    pub fn retained_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(
                self.columns
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Column>()),
            )
            .saturating_add(
                self.rows
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Vec<Cell>>()),
            );
        for column in &self.columns {
            bytes = bytes.saturating_add(column.path.capacity());
        }
        for row in &self.rows {
            bytes =
                bytes.saturating_add(row.capacity().saturating_mul(std::mem::size_of::<Cell>()));
            for cell in row {
                bytes = bytes.saturating_add(cell.text.capacity());
            }
        }
        bytes
    }

    /// 在左侧插入与原行对齐的祖先列。
    pub fn prepend_lead(&mut self, mut lead: Vec<Column>, lead_rows: Vec<Vec<Cell>>) {
        if lead.is_empty() {
            return;
        }
        let discovered = lead.len();
        self.total_columns = self.total_columns.saturating_add(discovered);
        let n = self.lead_capacity().min(discovered);
        if n == 0 {
            return;
        }
        // 预算不足时优先保留更近的祖先。
        lead.drain(..discovered - n);
        let empty = Cell {
            text: String::new(),
            kind: "null",
        };
        let mut cols = lead;
        cols.append(&mut self.columns);
        self.columns = cols;
        for (i, row) in self.rows.iter_mut().enumerate() {
            let mut head = lead_rows.get(i).cloned().unwrap_or_default();
            if head.len() > n {
                head.drain(..head.len() - n);
            }
            head.resize(n, empty.clone());
            head.append(row);
            *row = head;
        }
    }

    /// 直接写入各行，避免构造完整祖先矩阵。
    pub fn prepend_constant_lead(&mut self, lead: Vec<(Column, Cell)>) {
        self.prepend_constant_lead_impl(lead, None);
    }

    /// 返回 `false` 表示任务已取消。
    pub fn prepend_constant_lead_cancellable(
        &mut self,
        lead: Vec<(Column, Cell)>,
        cancelled: &AtomicBool,
    ) -> bool {
        self.prepend_constant_lead_impl(lead, Some(cancelled))
    }

    fn prepend_constant_lead_impl(
        &mut self,
        lead: Vec<(Column, Cell)>,
        cancelled: Option<&AtomicBool>,
    ) -> bool {
        if lead.is_empty() {
            return true;
        }
        let discovered = lead.len();
        self.total_columns = self.total_columns.saturating_add(discovered);
        let n = self.lead_capacity().min(discovered);
        if n == 0 {
            return true;
        }
        let kept = lead.into_iter().skip(discovered - n);
        let (mut columns, cells): (Vec<_>, Vec<_>) = kept.unzip();
        columns.append(&mut self.columns);
        self.columns = columns;
        for (index, row) in self.rows.iter_mut().enumerate() {
            if index % 64 == 0 && is_cancelled(cancelled) {
                return false;
            }
            let mut head = cells.clone();
            head.append(row);
            *row = head;
        }
        true
    }

    fn lead_capacity(&self) -> usize {
        table_column_limit(self.rows.len()).saturating_sub(self.columns.len())
    }
}

#[cfg(test)]
fn build_flat_table(docs: &[Value]) -> FlatTable {
    build_flat_table_with(docs, &BTreeSet::new())
}

/// 将指定对象路径递归展开为点分列。
#[cfg(test)]
pub fn build_flat_table_with(docs: &[Value], expanded: &BTreeSet<String>) -> FlatTable {
    build_flat_table_impl(docs, expanded, None).unwrap_or_default()
}

/// 新结果到来后可取消旧表格构建。
pub fn build_flat_table_with_cancellable(
    docs: &[Value],
    expanded: &BTreeSet<String>,
    cancelled: &AtomicBool,
) -> Option<FlatTable> {
    build_flat_table_impl(docs, expanded, Some(cancelled))
}

fn build_flat_table_impl(
    docs: &[Value],
    expanded: &BTreeSet<String>,
    cancelled: Option<&AtomicBool>,
) -> Option<FlatTable> {
    let column_limit = table_column_limit(docs.len());
    let mut col_seen: HashSet<String> = HashSet::new();
    let mut col_order: Vec<String> = Vec::new();
    let mut col_kinds: HashMap<String, &'static str> = HashMap::new();
    let mut columns_truncated = false;
    // 达到列预算后不再构造中间单元格。
    let mut flat_rows: Vec<BTreeMap<String, Cell>> = Vec::with_capacity(docs.len());
    for (index, document) in docs.iter().enumerate() {
        if index % 64 == 0 && is_cancelled(cancelled) {
            return None;
        }
        flat_rows.push(flatten_doc_bounded(
            document,
            expanded,
            column_limit,
            &mut col_seen,
            &mut col_order,
            &mut col_kinds,
            &mut columns_truncated,
        ));
    }

    col_order.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        ("_id", _) => std::cmp::Ordering::Less,
        (_, "_id") => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });

    let total_columns = col_order
        .len()
        .saturating_add(usize::from(columns_truncated));
    let columns: Vec<Column> = col_order
        .iter()
        .map(|p| Column {
            path: p.clone(),
            kind: col_kinds.get(p).copied().unwrap_or("null"),
        })
        .collect();

    let empty_cell = Cell {
        text: String::new(),
        kind: "null",
    };
    let mut rows = Vec::with_capacity(flat_rows.len());
    for (index, mut row) in flat_rows.into_iter().enumerate() {
        if index % 64 == 0 && is_cancelled(cancelled) {
            return None;
        }
        rows.push(
            columns
                .iter()
                .map(|column| {
                    row.remove(&column.path)
                        .unwrap_or_else(|| empty_cell.clone())
                })
                .collect(),
        );
    }

    Some(FlatTable {
        columns,
        total_columns,
        rows,
    })
}

fn is_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|token| token.load(Ordering::Relaxed))
}

fn table_column_limit(row_count: usize) -> usize {
    let cell_limited = MAX_TABLE_CELLS / row_count.max(1);
    MAX_TABLE_COLUMNS.min(cell_limited.max(1))
}

/// 只构造可展示列，其余字段标记为已裁剪。
#[allow(clippy::too_many_arguments)]
fn flatten_doc_bounded(
    value: &Value,
    expanded: &BTreeSet<String>,
    column_limit: usize,
    col_seen: &mut HashSet<String>,
    col_order: &mut Vec<String>,
    col_kinds: &mut HashMap<String, &'static str>,
    columns_truncated: &mut bool,
) -> BTreeMap<String, Cell> {
    let mut out = BTreeMap::new();
    match value {
        // ExtJSON 包装（$oid 等）是标量而非子文档：走 _value 单列，避免拆出字面 $oid 列
        Value::Object(map) if extjson_cell(map).is_none() => flatten_into_bounded(
            map,
            "",
            expanded,
            column_limit,
            col_seen,
            col_order,
            col_kinds,
            columns_truncated,
            &mut out,
        ),
        _ => {
            insert_bounded_cell(
                "_value".to_string(),
                value,
                column_limit,
                col_seen,
                col_order,
                col_kinds,
                columns_truncated,
                &mut out,
            );
        }
    }
    out
}

/// 展开指定普通对象；其余嵌套值保留摘要列。
#[allow(clippy::too_many_arguments)]
fn flatten_into_bounded(
    map: &serde_json::Map<String, Value>,
    prefix: &str,
    expanded: &BTreeSet<String>,
    column_limit: usize,
    col_seen: &mut HashSet<String>,
    col_order: &mut Vec<String>,
    col_kinds: &mut HashMap<String, &'static str>,
    columns_truncated: &mut bool,
    out: &mut BTreeMap<String, Cell>,
) {
    if prefix.is_empty()
        && let Some(value) = map.get("_id")
    {
        flatten_value_bounded(
            "_id".to_string(),
            value,
            expanded,
            column_limit,
            col_seen,
            col_order,
            col_kinds,
            columns_truncated,
            out,
        );
    }
    for (k, vv) in map {
        if prefix.is_empty() && k == "_id" {
            continue;
        }
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        flatten_value_bounded(
            path,
            vv,
            expanded,
            column_limit,
            col_seen,
            col_order,
            col_kinds,
            columns_truncated,
            out,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten_value_bounded(
    path: String,
    value: &Value,
    expanded: &BTreeSet<String>,
    column_limit: usize,
    col_seen: &mut HashSet<String>,
    col_order: &mut Vec<String>,
    col_kinds: &mut HashMap<String, &'static str>,
    columns_truncated: &mut bool,
    out: &mut BTreeMap<String, Cell>,
) {
    match value {
        Value::Object(child) if expanded.contains(&path) && extjson_cell(child).is_none() => {
            flatten_into_bounded(
                child,
                &path,
                expanded,
                column_limit,
                col_seen,
                col_order,
                col_kinds,
                columns_truncated,
                out,
            );
        }
        _ => insert_bounded_cell(
            path,
            value,
            column_limit,
            col_seen,
            col_order,
            col_kinds,
            columns_truncated,
            out,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_bounded_cell(
    path: String,
    value: &Value,
    column_limit: usize,
    col_seen: &mut HashSet<String>,
    col_order: &mut Vec<String>,
    col_kinds: &mut HashMap<String, &'static str>,
    columns_truncated: &mut bool,
    out: &mut BTreeMap<String, Cell>,
) {
    if !col_seen.contains(&path) {
        if col_seen.len() >= column_limit {
            *columns_truncated = true;
            return;
        }
        col_seen.insert(path.clone());
        col_order.push(path.clone());
    }
    let cell = cell_for_value(value);
    let kind = col_kinds.entry(path.clone()).or_insert(cell.kind);
    if *kind == "null" && cell.kind != "null" {
        *kind = cell.kind;
    }
    out.insert(path, cell);
}

/// 收集有限深度的点分字段补全候选。
#[cfg(test)]
pub fn collect_paths(docs: &[Value], max_depth: usize) -> Vec<String> {
    collect_paths_impl(docs, max_depth, None).unwrap_or_default()
}

/// 新结果到来后可取消旧补全遍历。
pub fn collect_paths_cancellable(
    docs: &[Value],
    max_depth: usize,
    cancelled: &AtomicBool,
) -> Option<Vec<String>> {
    collect_paths_impl(docs, max_depth, Some(cancelled))
}

fn collect_paths_impl(
    docs: &[Value],
    max_depth: usize,
    cancelled: Option<&AtomicBool>,
) -> Option<Vec<String>> {
    let mut set = BTreeSet::new();
    for (index, doc) in docs.iter().enumerate() {
        if index % 64 == 0 && is_cancelled(cancelled) {
            return None;
        }
        if let Value::Object(map) = doc {
            collect_into(map, "", max_depth, &mut set);
        }
    }
    Some(set.into_iter().collect())
}

fn collect_into(
    map: &serde_json::Map<String, Value>,
    prefix: &str,
    depth: usize,
    out: &mut BTreeSet<String>,
) {
    for (k, vv) in map {
        if out.len() >= MAX_COMPLETION_PATHS {
            return;
        }
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        out.insert(path.clone());
        if depth <= 1 {
            continue;
        }
        match vv {
            Value::Object(child) if extjson_cell(child).is_none() => {
                collect_into(child, &path, depth - 1, out);
            }
            // 数组只采样首个对象元素。
            Value::Array(arr) => {
                if let Some(Value::Object(child)) = arr.iter().find(|e| e.is_object()) {
                    collect_into(child, &path, depth - 1, out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
