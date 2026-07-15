//! 文档扁平化：只解析第一层字段（列 = 顶层 key）。嵌套对象/数组出摘要，完整内容靠表格双击查看；
//! Extended JSON 包装类型（$oid / $numberDecimal / $date / $binary）取内部值
//!
//! 例：
//!   `{"specs":{"cpu":"i7"}}` → `{"specs": "{1 字段}"}`（不再展开成 specs.cpu）
//!   `{"tags":["x","y"]}` → `{"tags": "[2 项]"}`
//!   `{"_id":{"$oid":"abc..."}}` → `{"_id": "abc..."}`

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::Value;

use super::cell::{Cell, cell_for_value, extjson_cell};

const MAX_TABLE_COLUMNS: usize = 512;
const MAX_TABLE_CELLS: usize = 2_000_000;
const MAX_COMPLETION_PATHS: usize = 10_000;

/// 单列元信息
#[derive(Debug, Clone)]
pub struct Column {
    /// dotted path
    pub path: String,
    /// 类型：取该列下首个非 null 的 kind
    pub kind: &'static str,
}

/// 扁平化的表格
#[derive(Debug, Clone, Default)]
pub struct FlatTable {
    pub columns: Vec<Column>,
    /// 截断前发现的联合列总数；大于 columns.len() 表示表格矩阵按资源预算裁剪。
    pub total_columns: usize,
    /// 每行 = 列对齐的 cell 字符串（缺字段填空字符串，kind=null）
    pub rows: Vec<Vec<Cell>>,
}

impl FlatTable {
    /// 在最左插入前导列（下钻时展示祖先文档 id）。`lead_rows[i]` 与第 i 行对齐、
    /// 长度与 `lead` 一致；行数不足处补空。lead 为空则不动
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
        // 预算不足时保留更接近当前层的祖先列。
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

    /// 下钻层的祖先值对所有行相同；直接逐行写入，避免先构造完整 `lead_rows` 中间矩阵。
    pub fn prepend_constant_lead(&mut self, lead: Vec<(Column, Cell)>) {
        if lead.is_empty() {
            return;
        }
        let discovered = lead.len();
        self.total_columns = self.total_columns.saturating_add(discovered);
        let n = self.lead_capacity().min(discovered);
        if n == 0 {
            return;
        }
        let kept = lead.into_iter().skip(discovered - n);
        let (mut columns, cells): (Vec<_>, Vec<_>) = kept.unzip();
        columns.append(&mut self.columns);
        self.columns = columns;
        for row in &mut self.rows {
            let mut head = cells.clone();
            head.append(row);
            *row = head;
        }
    }

    fn lead_capacity(&self) -> usize {
        table_column_limit(self.rows.len()).saturating_sub(self.columns.len())
    }
}

/// 测试便捷入口：不展开（等价 build_flat_table_with 传空集）
#[cfg(test)]
fn build_flat_table(docs: &[Value]) -> FlatTable {
    build_flat_table_with(docs, &BTreeSet::new())
}

/// 带展开路径的扁平化：expanded 里的对象路径递归展开成 `父.子` 子列（array 不展开，仍走 unwind）
pub fn build_flat_table_with(docs: &[Value], expanded: &BTreeSet<String>) -> FlatTable {
    // 1) 扁平化每条文档
    let flat_rows: Vec<BTreeMap<String, Cell>> =
        docs.iter().map(|d| flatten_doc(d, expanded)).collect();

    // 2) 列发现 + 类型推断
    let mut col_seen: HashSet<String> = HashSet::new();
    let mut col_order: Vec<String> = Vec::new();
    let mut col_kinds: HashMap<String, &'static str> = HashMap::new();
    for row in &flat_rows {
        for (k, v) in row {
            if col_seen.insert(k.clone()) {
                col_order.push(k.clone());
            }
            // 取首个非 null kind 作为该列类型
            col_kinds.entry(k.clone()).or_insert(v.kind);
            if let Some(existing) = col_kinds.get_mut(k)
                && *existing == "null"
                && v.kind != "null"
            {
                *existing = v.kind;
            }
        }
    }

    // 3) 排序：_id 优先；其它按插入顺序
    col_order.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        ("_id", _) => std::cmp::Ordering::Less,
        (_, "_id") => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });

    let total_columns = col_order.len();
    col_order.truncate(table_column_limit(docs.len()));
    let columns: Vec<Column> = col_order
        .iter()
        .map(|p| Column {
            path: p.clone(),
            kind: col_kinds.get(p).copied().unwrap_or("null"),
        })
        .collect();

    // 4) 行 → 列对齐
    let empty_cell = Cell {
        text: String::new(),
        kind: "null",
    };
    let rows: Vec<Vec<Cell>> = flat_rows
        .into_iter()
        .map(|mut row| {
            columns
                .iter()
                .map(|c| row.remove(&c.path).unwrap_or_else(|| empty_cell.clone()))
                .collect()
        })
        .collect();

    FlatTable {
        columns,
        total_columns,
        rows,
    }
}

fn table_column_limit(row_count: usize) -> usize {
    let cell_limited = MAX_TABLE_CELLS / row_count.max(1);
    MAX_TABLE_COLUMNS.min(cell_limited.max(1))
}

/// 扁平化单文档：默认只解析第一层；expanded 含某对象路径则递归展开成 dotted-path 子列
fn flatten_doc(v: &Value, expanded: &BTreeSet<String>) -> BTreeMap<String, Cell> {
    let mut out = BTreeMap::new();
    match v {
        Value::Object(map) => flatten_into(map, "", expanded, &mut out),
        _ => {
            out.insert("_value".to_string(), cell_for_value(v));
        }
    }
    out
}

/// 递归展开：path 在 expanded 且值为普通对象（排除 $oid 等 ExtJSON 包装）→ 展开成 path.child 子列；
/// 否则该字段作为单列（嵌套对象仍出 `{N 字段}` 摘要）。prefix 空表示顶层
fn flatten_into(
    map: &serde_json::Map<String, Value>,
    prefix: &str,
    expanded: &BTreeSet<String>,
    out: &mut BTreeMap<String, Cell>,
) {
    for (k, vv) in map {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match vv {
            Value::Object(child) if expanded.contains(&path) && extjson_cell(child).is_none() => {
                flatten_into(child, &path, expanded, out);
            }
            _ => {
                out.insert(path, cell_for_value(vv));
            }
        }
    }
}

/// 收集过滤列补全候选：顶层字段 + 嵌套对象的 dotted 子字段路径（到 max_depth 层）。
/// 让「consume.」能补全出 consume.cost；array 与 ExtJSON 包装不深入
pub fn collect_paths(docs: &[Value], max_depth: usize) -> Vec<String> {
    let mut set = BTreeSet::new();
    for doc in docs {
        if let Value::Object(map) = doc {
            collect_into(map, "", max_depth, &mut set);
        }
    }
    set.into_iter().collect()
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
            // 数组：采样首个对象元素，按同前缀收集（jobs → jobs.connectors）
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flatten_scalar_object() {
        let t = build_flat_table(&[json!({"a": 1, "b": "x"})]);
        assert_eq!(t.columns.len(), 2);
        let a = t.columns.iter().find(|c| c.path == "a").unwrap();
        assert_eq!(a.kind, "int");
    }

    #[test]
    fn sparse_column_matrix_is_bounded() {
        let docs: Vec<_> = (0..(MAX_TABLE_COLUMNS + 1))
            .map(|index| json!({(format!("field_{index}")): index}))
            .collect();

        let table = build_flat_table(&docs);

        assert_eq!(table.total_columns, MAX_TABLE_COLUMNS + 1);
        assert_eq!(table.columns.len(), MAX_TABLE_COLUMNS);
        assert_eq!(table.rows.len(), docs.len());
        assert_eq!(table_column_limit(50_000), 40);
    }

    #[test]
    fn flatten_nested_object_is_summary() {
        let t = build_flat_table(&[json!({"specs": {"cpu": "i7", "ram": 16}})]);
        // 只解析第一层：specs 是一列摘要，不展开成 specs.cpu / specs.ram
        assert_eq!(t.columns.len(), 1);
        assert_eq!(t.columns[0].path, "specs");
        assert_eq!(t.columns[0].kind, "object");
        assert_eq!(t.rows[0][0].text, "{2 字段}");
    }

    #[test]
    fn flatten_oid_unwrapped() {
        let t = build_flat_table(&[json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}})]);
        let cell = &t.rows[0][0];
        assert_eq!(cell.kind, "oid");
        assert_eq!(cell.text, "507f1f77bcf86cd799439011");
    }

    #[test]
    fn flatten_decimal_unwrapped() {
        let t = build_flat_table(&[json!({"price": {"$numberDecimal": "1299.99"}})]);
        assert_eq!(t.rows[0][0].kind, "decimal");
        assert_eq!(t.rows[0][0].text, "1299.99");
    }

    #[test]
    fn flatten_array_is_summary() {
        let t = build_flat_table(&[json!({"tags": ["a", "b"]})]);
        assert_eq!(t.rows[0][0].kind, "array");
        assert_eq!(t.rows[0][0].text, "[2 项]");
    }

    #[test]
    fn flatten_columns_id_first() {
        let t = build_flat_table(&[json!({"name": "a", "_id": "x"})]);
        assert_eq!(t.columns[0].path, "_id");
    }

    #[test]
    fn flatten_missing_field_filled_null() {
        let t = build_flat_table(&[json!({"a": 1}), json!({"b": 2})]);
        assert_eq!(t.columns.len(), 2);
        assert_eq!(t.rows[0].len(), 2);
        assert_eq!(t.rows[1].len(), 2);
    }

    #[test]
    fn flatten_date_canonical_form() {
        // canonical {$date: {$numberLong: "ms"}}
        let t = build_flat_table(&[json!({"ts": {"$date": {"$numberLong": "1700000000000"}}})]);
        assert_eq!(t.rows[0][0].kind, "date");
        assert_eq!(t.rows[0][0].text, "1700000000000");
    }

    #[test]
    fn flatten_timestamp() {
        let t = build_flat_table(&[json!({"ts": {"$timestamp": {"t": 1700, "i": 5}}})]);
        assert_eq!(t.rows[0][0].kind, "ts");
        assert!(t.rows[0][0].text.contains("1700"));
    }

    #[test]
    fn flatten_regex() {
        let t = build_flat_table(&[json!({
            "rx": {"$regularExpression": {"pattern": "^abc", "options": "i"}}
        })]);
        assert_eq!(t.rows[0][0].kind, "regex");
        assert_eq!(t.rows[0][0].text, "/^abc/i");
    }

    #[test]
    fn flatten_minkey_maxkey() {
        let t = build_flat_table(&[json!({"lo": {"$minKey": 1}, "hi": {"$maxKey": 1}})]);
        let lo = t
            .columns
            .iter()
            .position(|c| c.path == "lo")
            .map(|i| &t.rows[0][i])
            .unwrap();
        let hi = t
            .columns
            .iter()
            .position(|c| c.path == "hi")
            .map(|i| &t.rows[0][i])
            .unwrap();
        assert_eq!(lo.kind, "minkey");
        assert_eq!(hi.kind, "maxkey");
    }

    #[test]
    fn flatten_undefined() {
        let t = build_flat_table(&[json!({"x": {"$undefined": true}})]);
        assert_eq!(t.rows[0][0].kind, "undef");
        assert_eq!(t.rows[0][0].text, "undefined");
    }

    #[test]
    fn flatten_code_and_symbol() {
        let t = build_flat_table(&[json!({
            "fn": {"$code": "function(){}"},
            "sym": {"$symbol": "alpha"}
        })]);
        let f = t
            .columns
            .iter()
            .position(|c| c.path == "fn")
            .map(|i| &t.rows[0][i])
            .unwrap();
        let s = t
            .columns
            .iter()
            .position(|c| c.path == "sym")
            .map(|i| &t.rows[0][i])
            .unwrap();
        assert_eq!(f.kind, "code");
        assert_eq!(s.kind, "symbol");
    }

    #[test]
    fn flatten_int32_canonical() {
        let t = build_flat_table(&[json!({"n": {"$numberInt": "42"}})]);
        assert_eq!(t.rows[0][0].kind, "int");
        assert_eq!(t.rows[0][0].text, "42");
    }

    #[test]
    fn flatten_int64_numberlong_is_long() {
        // Int64（driver 包装成 $numberLong）→ 独立 "long" kind，当标量不误判嵌套
        let t = build_flat_table(&[json!({"n": {"$numberLong": "9999999999"}})]);
        assert_eq!(t.rows[0][0].kind, "long");
        assert_eq!(t.rows[0][0].text, "9999999999");
    }

    #[test]
    fn flatten_double_canonical() {
        let t = build_flat_table(&[json!({"d": {"$numberDouble": "Infinity"}})]);
        assert_eq!(t.rows[0][0].kind, "double");
        assert_eq!(t.rows[0][0].text, "Infinity");
    }

    #[test]
    fn flatten_binary_with_subtype() {
        let t = build_flat_table(&[json!({
            "blob": {"$binary": {"base64": "aGVsbG8=", "subType": "00"}}
        })]);
        assert_eq!(t.rows[0][0].kind, "binary");
        assert!(t.rows[0][0].text.contains("subType=00"));
    }

    #[test]
    fn expand_object_path_into_subcolumns() {
        let docs = vec![json!({"consume": {"cost": 12, "name": "x"}, "id": 1})];
        let exp = BTreeSet::from(["consume".to_string()]);
        let t = build_flat_table_with(&docs, &exp);
        // consume 展开成 consume.cost / consume.name，不再是 {N 字段} 摘要
        assert!(t.columns.iter().any(|c| c.path == "consume.cost"));
        assert!(t.columns.iter().any(|c| c.path == "consume.name"));
        assert!(!t.columns.iter().any(|c| c.path == "consume"));
    }

    #[test]
    fn expand_nested_two_levels() {
        let docs = vec![json!({"a": {"b": {"c": 1}}})];
        let exp = BTreeSet::from(["a".to_string(), "a.b".to_string()]);
        let t = build_flat_table_with(&docs, &exp);
        assert!(t.columns.iter().any(|c| c.path == "a.b.c"));
    }

    #[test]
    fn expand_skips_extjson_wrapper() {
        // _id 是 $oid 包装，即使在 expanded 也按标量取值，不展开成 _id.$oid
        let docs = vec![json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}})];
        let exp = BTreeSet::from(["_id".to_string()]);
        let t = build_flat_table_with(&docs, &exp);
        assert_eq!(t.rows[0][0].kind, "oid");
        assert_eq!(t.rows[0][0].text, "507f1f77bcf86cd799439011");
    }

    #[test]
    fn no_expand_keeps_summary() {
        // 不传展开路径 → 维持现有「第一层摘要」行为
        let t = build_flat_table(&[json!({"consume": {"cost": 12}})]);
        assert_eq!(t.rows[0][0].kind, "object");
        assert_eq!(t.rows[0][0].text, "{1 字段}");
    }

    #[test]
    fn collect_paths_includes_nested() {
        let docs = vec![json!({"consume": {"cost": 1, "detail": {"x": 2}}, "id": 1})];
        let paths = collect_paths(&docs, 4);
        for want in [
            "consume",
            "consume.cost",
            "consume.detail",
            "consume.detail.x",
            "id",
        ] {
            assert!(paths.contains(&want.to_string()), "missing {want}");
        }
    }

    #[test]
    fn collect_paths_skips_extjson() {
        // $oid 包装不深入成 _id.$oid
        let docs = vec![json!({"_id": {"$oid": "abc"}})];
        let paths = collect_paths(&docs, 4);
        assert!(paths.contains(&"_id".to_string()));
        assert!(!paths.iter().any(|p| p.contains("$oid")));
    }

    #[test]
    fn collect_paths_through_array() {
        // 数组采样首个对象元素穿透收集（jobs → jobs.connectors）
        let docs = vec![json!({"jobs": [{"connectors": {"x": 1}, "cover": 2}]})];
        let paths = collect_paths(&docs, 5);
        for want in ["jobs", "jobs.connectors", "jobs.cover", "jobs.connectors.x"] {
            assert!(paths.contains(&want.to_string()), "missing {want}");
        }
    }

    #[test]
    fn prepend_lead_inserts_leading_columns() {
        let mut t = build_flat_table(&[json!({"a": 1}), json!({"a": 2})]);
        let lead = vec![Column {
            path: "‹父1›".to_string(),
            kind: "text",
        }];
        let lead_rows = vec![
            vec![Cell {
                text: "p1".to_string(),
                kind: "text",
            }],
            vec![Cell {
                text: "p2".to_string(),
                kind: "text",
            }],
        ];
        t.prepend_lead(lead, lead_rows);
        // 前导列在最左，原列保留在后
        assert_eq!(t.columns[0].path, "‹父1›");
        assert!(t.columns.iter().any(|c| c.path == "a"));
        assert_eq!(t.rows[0][0].text, "p1");
        assert_eq!(t.rows[1][0].text, "p2");
        // 每行列数 = 前导 1 + 原 1
        assert_eq!(t.rows[0].len(), t.columns.len());
    }

    #[test]
    fn constant_lead_shares_column_and_cell_budget() {
        let empty = Cell {
            text: String::new(),
            kind: "null",
        };
        let mut table = FlatTable {
            columns: (0..MAX_TABLE_COLUMNS)
                .map(|index| Column {
                    path: format!("c{index}"),
                    kind: "text",
                })
                .collect(),
            total_columns: MAX_TABLE_COLUMNS,
            rows: vec![vec![empty; MAX_TABLE_COLUMNS]],
        };

        table.prepend_constant_lead(vec![(
            Column {
                path: "parent".into(),
                kind: "text",
            },
            Cell {
                text: "id".into(),
                kind: "text",
            },
        )]);

        assert_eq!(table.columns.len(), MAX_TABLE_COLUMNS);
        assert_eq!(table.rows[0].len(), MAX_TABLE_COLUMNS);
        assert_eq!(table.total_columns, MAX_TABLE_COLUMNS + 1);
    }
}
