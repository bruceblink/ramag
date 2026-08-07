//! SQL 同步身份键、谓词和行序列化。

use super::*;

pub(super) fn keyset_select(
    driver: DriverKind,
    table: &str,
    selected_columns: &str,
    identity_columns: &[String],
    last_key: Option<&[Value]>,
) -> String {
    let order = quoted_columns(driver, identity_columns);
    let predicate = last_key.map_or(String::new(), |values| {
        let literals = values
            .iter()
            .map(|value| transfer_literal(value, driver))
            .collect::<Vec<_>>()
            .join(", ");
        if identity_columns.len() == 1 {
            format!(
                " WHERE {} > {literals}",
                driver.quote_identifier(&identity_columns[0])
            )
        } else {
            format!(" WHERE ({order}) > ({literals})")
        }
    });
    format!(
        "SELECT {selected_columns} FROM {table}{predicate} ORDER BY {order} LIMIT {TRANSFER_BATCH_ITEMS};"
    )
}

pub(super) fn identity_predicate(
    driver: DriverKind,
    columns: &[String],
    rows: &[Row],
) -> Result<String> {
    if rows.is_empty() {
        return Err(DomainError::Other("身份键批次不能为空".into()));
    }
    for row in rows {
        if row.values.len() != columns.len() {
            return Err(DomainError::QueryFailed("身份键列数与结果不一致".into()));
        }
    }
    if columns.len() == 1 {
        let values = rows
            .iter()
            .map(|row| transfer_literal(&row.values[0], driver))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "{} IN ({values})",
            driver.quote_identifier(&columns[0])
        ))
    } else {
        let values = rows
            .iter()
            .map(|row| {
                format!(
                    "({})",
                    row.values
                        .iter()
                        .map(|value| transfer_literal(value, driver))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "({}) IN ({values})",
            quoted_columns(driver, columns)
        ))
    }
}

pub(super) fn identity_ranges(
    driver: DriverKind,
    identities: &[Row],
) -> Result<Vec<std::ops::Range<usize>>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0usize;
    for (index, row) in identities.iter().enumerate() {
        let row_bytes = row
            .values
            .iter()
            .map(|value| transfer_literal(value, driver).len().saturating_add(2))
            .sum::<usize>();
        if row_bytes > TRANSFER_BATCH_BYTES {
            return Err(DomainError::InvalidConfig(
                "单个 SQL 身份键超过安全字节上限".into(),
            ));
        }
        if index > start
            && (index - start >= ID_QUERY_ITEMS || bytes.saturating_add(row_bytes) > ID_QUERY_BYTES)
        {
            ranges.push(start..index);
            start = index;
            bytes = 0;
        }
        bytes = bytes.saturating_add(row_bytes);
    }
    if start < identities.len() {
        ranges.push(start..identities.len());
    }
    Ok(ranges)
}

pub(super) fn identity_positions(object: &SqlPreparedObject) -> Result<Vec<usize>> {
    object
        .identity
        .columns
        .iter()
        .map(|name| {
            object
                .writable_columns
                .iter()
                .position(|column| column == name)
                .ok_or_else(|| DomainError::InvalidConfig(format!("记录身份列 {name} 不可写入")))
        })
        .collect()
}

pub(super) fn last_identity(
    result: &QueryResult,
    positions: &[usize],
    object: &SqlPreparedObject,
) -> Result<Vec<Value>> {
    let row = result
        .rows
        .last()
        .ok_or_else(|| DomainError::QueryFailed("SQL 分页结果为空".into()))?;
    positions
        .iter()
        .map(|position| {
            row.values.get(*position).cloned().ok_or_else(|| {
                DomainError::QueryFailed(format!("表 {} 的身份键结果列缺失", object.mapping.source))
            })
        })
        .collect()
}

pub(super) fn identity_key(values: &[Value]) -> Result<String> {
    serde_json::to_string(values)
        .map_err(|error| DomainError::Other(format!("序列化 SQL 身份键失败：{error}")))
}

pub(super) fn quoted_columns(driver: DriverKind, columns: &[String]) -> String {
    columns
        .iter()
        .map(|column| driver.quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn source_select_columns(
    driver: DriverKind,
    columns: &[String],
    text_columns: &HashSet<String>,
) -> String {
    columns
        .iter()
        .map(|column| {
            let quoted = driver.quote_identifier(column);
            if driver == DriverKind::Postgres && text_columns.contains(column) {
                format!("CAST({quoted} AS TEXT) AS {quoted}")
            } else {
                quoted
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn row_tuple(row: &Row, driver: DriverKind) -> String {
    format!(
        "({})",
        row.values
            .iter()
            .map(|value| transfer_literal(value, driver))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(super) fn add_scanned(
    rows: u64,
    bytes: u64,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) {
    progress.add_scanned(rows);
    progress.add_bytes(bytes);
    summary.scanned = summary.scanned.saturating_add(rows);
    summary.bytes = summary.bytes.saturating_add(bytes);
}

pub(super) fn object_label(object: &SqlPreparedObject) -> String {
    format!("{} → {}", object.mapping.source, object.mapping.target)
}

pub(super) fn oversized_row_error(object: &SqlPreparedObject) -> DomainError {
    DomainError::InvalidConfig(format!(
        "表 {} 的单行或身份键超过 {} MiB 安全上限",
        object.mapping.source,
        TRANSFER_BATCH_BYTES / 1024 / 1024
    ))
}
