//! ResultPanel 自由函数：行定位键推导 / WHERE 拼装 / 类型转换 / DML LIMIT 方言 / 表名提取 / 输入校验

use gpui::Entity;
use gpui_component::input::InputState;
use ramag_domain::entities::{Column, ColumnKind, Index, MAX_SQL_QUERY_BYTES, QueryResult, Value};

pub(super) const MAX_BATCH_DELETE_ROWS: usize = 500;
pub(super) const MAX_BATCH_DELETE_SQL_BYTES: usize = MAX_SQL_QUERY_BYTES;
pub(super) const MAX_PENDING_CELL_EDITS: usize = 500;

/// A staged cell keeps the value returned by the query so a rollback of the local
/// edit can restore the exact original value instead of reconstructing it from text.
pub(super) struct PendingCellEdit {
    pub(super) original: Value,
    pub(super) current: Value,
}

pub(super) fn reserve_batch_delete_sql_bytes(current: usize, added: usize) -> Option<usize> {
    current
        .checked_add(added)
        .filter(|total| *total <= MAX_BATCH_DELETE_SQL_BYTES)
}

/// Value deliberately has no derived equality because floating-point NaN needs an explicit rule.
pub(super) fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Int(left), Value::Int(right)) => left == right,
        (Value::Float(left), Value::Float(right)) => left.to_bits() == right.to_bits(),
        (Value::Text(left), Value::Text(right)) => left == right,
        (Value::Bytes(left), Value::Bytes(right)) => left == right,
        (Value::DateTime(left), Value::DateTime(right)) => left == right,
        (Value::Json(left), Value::Json(right)) => left == right,
        _ => false,
    }
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
        ColumnKind::Decimal if is_decimal_number(trimmed) => {
            // DECIMAL 不能先转 f64，否则大整数和高精度小数会在生成 SQL 前被舍入。
            // 保留原文本；数据库会把带引号的精确十进制字面量转换为目标列类型。
            Ok(Some(Value::Text(trimmed.to_string())))
        }
        ColumnKind::Decimal => Err(format!("不是合法十进制数值: {trimmed}")),
        ColumnKind::Float => trimmed
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

fn is_decimal_number(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    let mut mantissa_digits = 0usize;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        mantissa_digits += 1;
        index += 1;
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            mantissa_digits += 1;
            index += 1;
        }
    }
    if mantissa_digits == 0 {
        return false;
    }
    if matches!(bytes.get(index), Some(b'e') | Some(b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+') | Some(b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len()
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

/// 公开版本：用于 ops::stage_cell_update 同步本地 cell
pub(super) fn build_new_value(old: &Value, new_text: &str) -> Value {
    build_new_value_for_old(old, new_text)
}

/// 单行 DML LIMIT 子句。MySQL ` LIMIT 1` 防误删；PG / Redis / MongoDB 不支持，返回空
pub(super) fn dml_row_limit(driver: ramag_domain::entities::DriverKind) -> &'static str {
    match driver {
        ramag_domain::entities::DriverKind::Mysql => " LIMIT 1",
        ramag_domain::entities::DriverKind::Postgres
        | ramag_domain::entities::DriverKind::Sqlite
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
mod tests;
