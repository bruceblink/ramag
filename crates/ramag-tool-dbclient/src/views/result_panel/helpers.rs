//! ResultPanel 自由函数：行定位键推导 / WHERE 拼装 / 类型转换 / DML LIMIT 方言 / 表名提取 / 输入校验

use gpui::Entity;
use gpui_component::input::InputState;
use ramag_domain::entities::{Column, ColumnKind, Index, MAX_SQL_QUERY_BYTES, QueryResult, Value};

pub(super) const MAX_BATCH_DELETE_ROWS: usize = 500;
pub(super) const MAX_BATCH_DELETE_SQL_BYTES: usize = MAX_SQL_QUERY_BYTES;

pub(super) fn reserve_batch_delete_sql_bytes(current: usize, added: usize) -> Option<usize> {
    current
        .checked_add(added)
        .filter(|total| *total <= MAX_BATCH_DELETE_SQL_BYTES)
}

/// 新增草稿行。表名在 INSERT 时由 `extract_first_table_ref` 从 SQL 反推，与 UPDATE/DELETE 一致
pub(crate) struct PendingInsert {
    pub columns: Vec<Column>,
    pub inputs: Vec<Entity<InputState>>,
}

pub(super) struct BatchDeleteNotice {
    pub message: String,
    pub persistent: bool,
}

/// 批量删除成功路径的用户提示。异常影响多行或结果集已切换时必须持久展示。
pub(super) fn batch_delete_notice(
    successful_requests: usize,
    affected_rows: u64,
    not_matched: usize,
    anomalous_affected: Option<u64>,
    strategy: &str,
    same_result: bool,
) -> BatchDeleteNotice {
    let mut message = if let Some(anomalous) = anomalous_affected {
        format!(
            "已执行 {successful_requests} 个删除请求，共影响 {affected_rows} 行；其中一次 DELETE 异常影响 {anomalous} 行（{strategy}匹配），已停止后续删除，请重新查询核对"
        )
    } else {
        let mut message = format!("已删除 {affected_rows} 行（{strategy}匹配）");
        if not_matched > 0 {
            message.push_str(&format!("，{not_matched} 行未匹配"));
        }
        message
    };
    if !same_result {
        message.push_str("；当前结果已变化，请重新查询核对");
    }
    BatchDeleteNotice {
        message,
        persistent: anomalous_affected.is_some() || !same_result,
    }
}

/// 用户输入 → Value。Ok(Some)=有值、Ok(None)=留空且有 default 走 DB DEFAULT、Err=非法
pub(super) fn parse_value_for_kind(
    kind: ColumnKind,
    text: &str,
    nullable: bool,
    has_default: bool,
) -> Result<Option<Value>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        if nullable {
            return Ok(Some(Value::Null));
        }
        if has_default {
            return Ok(None);
        }
        return Err("必填".to_string());
    }
    if trimmed.eq_ignore_ascii_case("NULL") {
        if nullable {
            return Ok(Some(Value::Null));
        }
        return Err("不可为 NULL".to_string());
    }
    match kind {
        ColumnKind::Integer => trimmed
            .parse::<i64>()
            .map(|i| Some(Value::Int(i)))
            .map_err(|_| format!("不是合法整数: {trimmed}")),
        ColumnKind::Decimal | ColumnKind::Float => trimmed
            .parse::<f64>()
            .map(|f| Some(Value::Float(f)))
            .map_err(|_| format!("不是合法数值: {trimmed}")),
        ColumnKind::Bool => match trimmed {
            "1" | "true" | "TRUE" | "True" => Ok(Some(Value::Bool(true))),
            "0" | "false" | "FALSE" | "False" => Ok(Some(Value::Bool(false))),
            _ => Err(format!("布尔值需 1/0/true/false: {trimmed}")),
        },
        _ => Ok(Some(Value::Text(trimmed.to_string()))),
    }
}

/// 行定位键：来自元数据的真实主键或全非空唯一索引（QueryTab 查询成功后异步注入）。
/// 行内修改 / 删除仅在拿到它后放行，绝不按列名猜测
#[derive(Clone, Debug)]
pub(crate) struct RowIdentity {
    /// 键列名（复合键保持索引列序）
    pub(crate) columns: Vec<String>,
    /// 定位方式提示文案："主键" / "唯一键"
    pub(crate) label: &'static str,
}

/// 从列元数据 + 索引推导行定位键：优先主键，其次首个全列非空的唯一索引
/// （可空唯一列允许多个 NULL，不能唯一定位行）；都没有返回 None → 调用方禁写
pub(crate) fn derive_row_identity(columns: &[Column], indexes: &[Index]) -> Option<RowIdentity> {
    let pk: Vec<String> = columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.clone())
        .collect();
    if !pk.is_empty() {
        return Some(RowIdentity {
            columns: pk,
            label: "主键",
        });
    }
    let non_nullable = |name: &str| {
        columns
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(name) && !c.nullable)
    };
    indexes
        .iter()
        .filter(|i| i.unique && !i.primary && !i.columns.is_empty())
        .find(|i| i.columns.iter().all(|c| non_nullable(c)))
        .map(|i| RowIdentity {
            columns: i.columns.clone(),
            label: "唯一键",
        })
}

/// 单列等值条件：NULL 用 `IS NULL`（`= NULL` 恒不成立会漏匹配），其余用 `col = 字面量`。
/// driver 决定标识符引号字符与字面量方言
fn col_eq_condition(
    col: &str,
    val: &ramag_domain::entities::Value,
    driver: ramag_domain::entities::DriverKind,
) -> String {
    use ramag_domain::entities::Value;
    let ident = driver.quote_identifier(col);
    if matches!(val, Value::Null) {
        format!("{ident} IS NULL")
    } else {
        format!("{ident} = {}", val.to_sql_literal_for(driver))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdentityWhereError {
    MissingColumn,
    TooLarge,
}

/// 构造按行定位键匹配单行的 WHERE：键列必须全部在结果集中（SELECT * 的单表数据必然满足），
/// 缺任一列返回 None，调用方拒绝执行而不是退化成模糊匹配
pub(super) fn build_identity_where(
    result: &QueryResult,
    row: &ramag_domain::entities::Row,
    identity: &RowIdentity,
    driver: ramag_domain::entities::DriverKind,
) -> Result<String, IdentityWhereError> {
    let mut values = Vec::with_capacity(identity.columns.len());
    let mut total_bytes = 0usize;
    for (position, col) in identity.columns.iter().enumerate() {
        let idx = result
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(col))
            .ok_or(IdentityWhereError::MissingColumn)?;
        let val = row
            .values
            .get(idx)
            .ok_or(IdentityWhereError::MissingColumn)?;
        let identifier_bytes = driver.quote_identifier(col).len();
        let separator_bytes = usize::from(position > 0) * " AND ".len();
        let condition_bytes = if matches!(val, Value::Null) {
            identifier_bytes.saturating_add(" IS NULL".len())
        } else {
            let fixed = identifier_bytes.saturating_add(" = ".len());
            let remaining = MAX_SQL_QUERY_BYTES
                .saturating_sub(total_bytes)
                .saturating_sub(separator_bytes)
                .saturating_sub(fixed);
            let literal = val
                .bounded_sql_literal_len_for(driver, remaining)
                .ok_or(IdentityWhereError::TooLarge)?;
            fixed.saturating_add(literal)
        };
        total_bytes = total_bytes
            .checked_add(separator_bytes)
            .and_then(|total| total.checked_add(condition_bytes))
            .filter(|total| *total <= MAX_SQL_QUERY_BYTES)
            .ok_or(IdentityWhereError::TooLarge)?;
        values.push((col.as_str(), val));
    }
    let mut output = String::with_capacity(total_bytes);
    for (position, (column, value)) in values.into_iter().enumerate() {
        if position > 0 {
            output.push_str(" AND ");
        }
        output.push_str(&col_eq_condition(column, value, driver));
    }
    Ok(output)
}

/// 按原 cell 类型把用户输入转换成新的 Value（同时供本地刷新 + SQL 字面量）
fn build_new_value_for_old(old: &Value, new_text: &str) -> Value {
    match old {
        Value::Null => {
            if new_text.is_empty() || new_text.eq_ignore_ascii_case("NULL") {
                Value::Null
            } else {
                Value::Text(new_text.to_string())
            }
        }
        Value::Int(_) => new_text
            .parse::<i64>()
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Text(new_text.to_string())),
        Value::Float(_) => new_text
            .parse::<f64>()
            .map(Value::Float)
            .unwrap_or_else(|_| Value::Text(new_text.to_string())),
        Value::Bool(_) => match new_text.trim() {
            "1" | "true" | "TRUE" | "True" => Value::Bool(true),
            "0" | "false" | "FALSE" | "False" => Value::Bool(false),
            _ => Value::Text(new_text.to_string()),
        },
        _ => Value::Text(new_text.to_string()),
    }
}

/// 公开版本：用于 ops::apply_cell_update_async 同步本地 cell
pub(super) fn build_new_value(old: &Value, new_text: &str) -> Value {
    build_new_value_for_old(old, new_text)
}

/// 单行 DML LIMIT 子句。MySQL ` LIMIT 1` 防误删；PG / Redis / MongoDB 不支持，返回空
pub(super) fn dml_row_limit(driver: ramag_domain::entities::DriverKind) -> &'static str {
    match driver {
        ramag_domain::entities::DriverKind::Mysql => " LIMIT 1",
        ramag_domain::entities::DriverKind::Postgres
        | ramag_domain::entities::DriverKind::Redis
        | ramag_domain::entities::DriverKind::Mongodb => "",
    }
}

/// 从 SQL 提取第一个表引用（按 driver 方言加引号），用于复制 INSERT 时的目标表
pub(super) fn extract_first_table_ref(
    sql: &str,
    driver: ramag_domain::entities::DriverKind,
) -> Option<String> {
    let tables = crate::sql_completion::extract_tables_in_use_for_prefetch(sql);
    let (maybe_schema, table) = tables.into_iter().next()?;
    let table_q = driver.quote_identifier(&table);
    Some(match maybe_schema {
        Some(s) => format!("{}.{}", driver.quote_identifier(&s), table_q),
        None => table_q,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{ColumnKind, DriverKind, QueryResult, Row, Value};

    /// 通过 `to_sql_literal` 把 Value 拍成可比较字符串（Value 没实现 PartialEq）
    fn lit(v: &Value) -> String {
        v.to_sql_literal()
    }

    fn make_result(cols: &[&str]) -> QueryResult {
        QueryResult {
            columns: cols.iter().map(|s| s.to_string()).collect(),
            column_types: vec![String::new(); cols.len()],
            rows: vec![],
            warnings: vec![],
            elapsed_ms: 0,
            affected_rows: 0,
            truncated: false,
        }
    }

    #[test]
    fn batch_delete_sql_budget_has_an_exact_boundary() {
        assert_eq!(
            reserve_batch_delete_sql_bytes(MAX_BATCH_DELETE_SQL_BYTES - 1, 1),
            Some(MAX_BATCH_DELETE_SQL_BYTES)
        );
        assert_eq!(
            reserve_batch_delete_sql_bytes(MAX_BATCH_DELETE_SQL_BYTES, 1),
            None
        );
    }

    #[test]
    fn parse_value_empty_nullable() {
        let v = parse_value_for_kind(ColumnKind::Text, "", true, false).unwrap();
        assert_eq!(lit(v.as_ref().unwrap()), "NULL");
    }

    #[test]
    fn parse_value_empty_with_default() {
        let v = parse_value_for_kind(ColumnKind::Text, "  ", false, true).unwrap();
        assert!(v.is_none(), "留空 + 有 default → 跳过让 DB 用 DEFAULT");
    }

    #[test]
    fn parse_value_empty_required() {
        let err = parse_value_for_kind(ColumnKind::Text, "", false, false).unwrap_err();
        assert!(err.contains("必填"));
    }

    #[test]
    fn parse_value_explicit_null_nullable() {
        for s in ["NULL", "null", "Null"] {
            let v = parse_value_for_kind(ColumnKind::Integer, s, true, false).unwrap();
            assert_eq!(lit(v.as_ref().unwrap()), "NULL", "input={s}");
        }
    }

    #[test]
    fn parse_value_explicit_null_not_nullable() {
        let err = parse_value_for_kind(ColumnKind::Integer, "NULL", false, true).unwrap_err();
        assert!(err.contains("不可为 NULL"));
    }

    #[test]
    fn parse_value_integer_ok() {
        let v = parse_value_for_kind(ColumnKind::Integer, "42", false, false).unwrap();
        assert_eq!(lit(v.as_ref().unwrap()), "42");
    }

    #[test]
    fn parse_value_integer_negative() {
        let v = parse_value_for_kind(ColumnKind::Integer, "-7", false, false).unwrap();
        assert_eq!(lit(v.as_ref().unwrap()), "-7");
    }

    #[test]
    fn parse_value_integer_invalid() {
        let err = parse_value_for_kind(ColumnKind::Integer, "abc", false, false).unwrap_err();
        assert!(err.contains("不是合法整数"));
    }

    #[test]
    fn parse_value_float_ok() {
        let v = parse_value_for_kind(ColumnKind::Float, "3.5", false, false).unwrap();
        assert!(matches!(v, Some(Value::Float(_))));
        assert_eq!(lit(v.as_ref().unwrap()), "3.5");
    }

    #[test]
    fn parse_value_decimal_ok() {
        let v = parse_value_for_kind(ColumnKind::Decimal, "1.5", false, false).unwrap();
        assert!(matches!(v, Some(Value::Float(_))));
        assert_eq!(lit(v.as_ref().unwrap()), "1.5");
    }

    #[test]
    fn parse_value_bool_truthy() {
        for s in ["1", "true", "TRUE", "True"] {
            let v = parse_value_for_kind(ColumnKind::Bool, s, false, false).unwrap();
            assert_eq!(lit(v.as_ref().unwrap()), "TRUE", "input={s}");
        }
    }

    #[test]
    fn parse_value_bool_falsy() {
        for s in ["0", "false", "FALSE", "False"] {
            let v = parse_value_for_kind(ColumnKind::Bool, s, false, false).unwrap();
            assert_eq!(lit(v.as_ref().unwrap()), "FALSE", "input={s}");
        }
    }

    #[test]
    fn parse_value_bool_invalid() {
        let err = parse_value_for_kind(ColumnKind::Bool, "yes", false, false).unwrap_err();
        assert!(err.contains("布尔值"));
    }

    #[test]
    fn parse_value_text_trimmed() {
        let v = parse_value_for_kind(ColumnKind::Text, "  hello  ", false, false).unwrap();
        assert_eq!(lit(v.as_ref().unwrap()), "'hello'");
    }

    /// 构造列元数据（name, nullable, is_pk）
    fn make_col(name: &str, nullable: bool, is_pk: bool) -> Column {
        Column {
            name: name.to_string(),
            data_type: ramag_domain::entities::ColumnType {
                kind: ColumnKind::Text,
                raw_type: "text".into(),
            },
            nullable,
            default_value: None,
            is_primary_key: is_pk,
            comment: None,
        }
    }

    fn make_index(name: &str, unique: bool, primary: bool, cols: &[&str]) -> Index {
        Index {
            name: name.to_string(),
            unique,
            primary,
            columns: cols.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn derive_identity_prefers_real_pk() {
        let cols = vec![
            make_col("uid", false, true),
            make_col("id", true, false), // 名叫 id 但不是主键：绝不能被选中
        ];
        let ident = derive_row_identity(&cols, &[]).unwrap();
        assert_eq!(ident.columns, vec!["uid".to_string()]);
        assert_eq!(ident.label, "主键");
    }

    #[test]
    fn derive_identity_composite_pk() {
        let cols = vec![
            make_col("order_id", false, true),
            make_col("item_id", false, true),
            make_col("qty", false, false),
        ];
        let ident = derive_row_identity(&cols, &[]).unwrap();
        assert_eq!(
            ident.columns,
            vec!["order_id".to_string(), "item_id".to_string()]
        );
    }

    #[test]
    fn derive_identity_falls_back_to_non_null_unique() {
        let cols = vec![
            make_col("email", false, false),
            make_col("name", true, false),
        ];
        let indexes = vec![make_index("uq_email", true, false, &["email"])];
        let ident = derive_row_identity(&cols, &indexes).unwrap();
        assert_eq!(ident.columns, vec!["email".to_string()]);
        assert_eq!(ident.label, "唯一键");
    }

    #[test]
    fn derive_identity_rejects_nullable_unique() {
        // 可空唯一列允许多个 NULL，不能唯一定位行
        let cols = vec![make_col("email", true, false)];
        let indexes = vec![make_index("uq_email", true, false, &["email"])];
        assert!(derive_row_identity(&cols, &indexes).is_none());
    }

    #[test]
    fn derive_identity_none_without_pk_or_unique() {
        let cols = vec![make_col("name", false, false)];
        let indexes = vec![make_index("idx_name", false, false, &["name"])];
        assert!(derive_row_identity(&cols, &indexes).is_none());
    }

    #[test]
    fn build_identity_where_single_pk_mysql() {
        let r = make_result(&["id", "name"]);
        let row = Row {
            values: vec![Value::Int(7), Value::Text("alice".into())],
        };
        let ident = RowIdentity {
            columns: vec!["id".into()],
            label: "主键",
        };
        let s = build_identity_where(&r, &row, &ident, DriverKind::Mysql).unwrap();
        assert_eq!(s, "`id` = 7");
    }

    #[test]
    fn build_identity_where_composite_postgres() {
        let r = make_result(&["order_id", "item_id", "qty"]);
        let row = Row {
            values: vec![Value::Int(1), Value::Int(2), Value::Int(3)],
        };
        let ident = RowIdentity {
            columns: vec!["order_id".into(), "item_id".into()],
            label: "主键",
        };
        let s = build_identity_where(&r, &row, &ident, DriverKind::Postgres).unwrap();
        assert_eq!(s, "\"order_id\" = 1 AND \"item_id\" = 2");
    }

    #[test]
    fn build_identity_where_missing_key_column_returns_error() {
        // 结果集缺键列（如用户只 SELECT 了部分列）：拒绝执行而不是模糊匹配
        let r = make_result(&["name"]);
        let row = Row {
            values: vec![Value::Text("a".into())],
        };
        let ident = RowIdentity {
            columns: vec!["id".into()],
            label: "主键",
        };
        assert_eq!(
            build_identity_where(&r, &row, &ident, DriverKind::Mysql),
            Err(IdentityWhereError::MissingColumn)
        );
    }

    #[test]
    fn build_identity_where_rejects_large_binary_key_before_hex_allocation() {
        let r = make_result(&["id"]);
        let row = Row {
            values: vec![Value::Bytes(vec![0; MAX_SQL_QUERY_BYTES / 2 + 1])],
        };
        let ident = RowIdentity {
            columns: vec!["id".into()],
            label: "主键",
        };

        assert_eq!(
            build_identity_where(&r, &row, &ident, DriverKind::Mysql),
            Err(IdentityWhereError::TooLarge)
        );
    }

    #[test]
    fn dml_row_limit_mysql() {
        assert_eq!(dml_row_limit(DriverKind::Mysql), " LIMIT 1");
    }

    #[test]
    fn dml_row_limit_postgres_empty() {
        assert_eq!(dml_row_limit(DriverKind::Postgres), "");
    }

    #[test]
    fn build_new_value_int_to_int() {
        let v = build_new_value_for_old(&Value::Int(0), "100");
        assert!(matches!(v, Value::Int(100)));
    }

    #[test]
    fn build_new_value_int_to_text_on_parse_fail() {
        let v = build_new_value_for_old(&Value::Int(0), "abc");
        assert_eq!(lit(&v), "'abc'");
    }

    #[test]
    fn build_new_value_null_with_empty_input() {
        let v = build_new_value_for_old(&Value::Null, "");
        assert!(matches!(v, Value::Null));
    }

    #[test]
    fn build_new_value_null_with_text() {
        let v = build_new_value_for_old(&Value::Null, "hello");
        assert_eq!(lit(&v), "'hello'");
    }

    #[test]
    fn batch_delete_notice_reports_misses_and_stale_results() {
        let notice = batch_delete_notice(2, 2, 1, None, "主键", false);

        assert!(notice.message.contains("2 行"));
        assert!(notice.message.contains("1 行未匹配"));
        assert!(notice.message.contains("当前结果已变化"));
        assert!(notice.persistent);
    }

    #[test]
    fn batch_delete_notice_stops_on_multi_row_anomaly() {
        let notice = batch_delete_notice(3, 5, 0, Some(3), "唯一键", true);

        assert!(notice.message.contains("异常影响 3 行"));
        assert!(notice.message.contains("已停止后续删除"));
        assert!(notice.persistent);
    }
}
