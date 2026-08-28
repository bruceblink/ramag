//! 未提交单元格修改的 SQL 生成、按行提交和失败反馈。

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_domain::entities::{DriverKind, MAX_SQL_QUERY_BYTES, Query, QueryResult, Row, Value};
use tracing::error;

use super::auto_commit_hint;
use crate::views::result_panel::helpers::{
    IdentityWhereError, RowIdentity, build_identity_where, dml_row_limit, values_equal,
};
use crate::views::result_panel::{ResultPanel, ResultPanelEvent, ResultState};

fn identity_where_error_message(error: IdentityWhereError) -> &'static str {
    match error {
        IdentityWhereError::MissingColumn => "结果集缺少定位键列，已取消操作；请重新查询该表",
        IdentityWhereError::TooLarge => {
            "行定位键生成的 SQL 超过 32 MiB 安全上限，已取消操作；请用手写 SQL 处理"
        }
    }
}

#[derive(Debug)]
pub(super) enum CellUpdateSqlError {
    Identity(IdentityWhereError),
    TooLarge,
}

pub(super) fn cell_update_sql_error_message(error: CellUpdateSqlError) -> String {
    match error {
        CellUpdateSqlError::Identity(error) => identity_where_error_message(error).to_string(),
        CellUpdateSqlError::TooLarge => format!(
            "UPDATE 生成的 SQL 超过 {} MiB 安全上限，请缩短单元格内容或减少同一行修改",
            MAX_SQL_QUERY_BYTES / 1024 / 1024
        ),
    }
}

/// Builds one bounded UPDATE so edits to several cells in the same row share one identity check.
pub(super) fn build_update_sql(
    result: &QueryResult,
    row: &Row,
    identity: &RowIdentity,
    table_ref: &str,
    edits: &[(String, Value)],
    driver: DriverKind,
) -> Result<String, CellUpdateSqlError> {
    let where_clause = build_identity_where(result, row, identity, driver)
        .map_err(CellUpdateSqlError::Identity)?;
    let limit_clause = dml_row_limit(driver);
    let mut sql_bytes = "UPDATE "
        .len()
        .checked_add(table_ref.len())
        .and_then(|bytes| bytes.checked_add(" SET ".len()))
        .and_then(|bytes| bytes.checked_add(" WHERE ".len()))
        .and_then(|bytes| bytes.checked_add(where_clause.len()))
        .and_then(|bytes| bytes.checked_add(limit_clause.len()))
        .and_then(|bytes| bytes.checked_add(1))
        .filter(|bytes| *bytes <= MAX_SQL_QUERY_BYTES)
        .ok_or(CellUpdateSqlError::TooLarge)?;

    let mut assignments = Vec::with_capacity(edits.len());
    for (index, (column, value)) in edits.iter().enumerate() {
        let quoted_column = driver.quote_identifier(column);
        sql_bytes = sql_bytes
            .checked_add(usize::from(index > 0) * 2)
            .and_then(|bytes| bytes.checked_add(quoted_column.len()))
            .and_then(|bytes| bytes.checked_add(" = ".len()))
            .filter(|bytes| *bytes <= MAX_SQL_QUERY_BYTES)
            .ok_or(CellUpdateSqlError::TooLarge)?;
        let remaining = MAX_SQL_QUERY_BYTES.saturating_sub(sql_bytes);
        let literal_bytes = value
            .bounded_sql_literal_len_for(driver, remaining)
            .ok_or(CellUpdateSqlError::TooLarge)?;
        sql_bytes = sql_bytes
            .checked_add(literal_bytes)
            .filter(|bytes| *bytes <= MAX_SQL_QUERY_BYTES)
            .ok_or(CellUpdateSqlError::TooLarge)?;
        assignments.push(format!(
            "{quoted_column} = {}",
            value.to_sql_literal_for(driver)
        ));
    }

    let sql = format!(
        "UPDATE {table_ref} SET {} WHERE {where_clause}{limit_clause};",
        assignments.join(", ")
    );
    (sql.len() <= MAX_SQL_QUERY_BYTES)
        .then_some(sql)
        .ok_or(CellUpdateSqlError::TooLarge)
}

struct PendingEditRequest {
    key: (usize, usize),
    column: String,
    value: Value,
}

struct PendingRowMutation {
    row_index: usize,
    edits: Vec<PendingEditRequest>,
    query: Query,
}

enum PendingBatchFailure {
    NotMatched {
        row_index: usize,
    },
    Anomalous {
        row_index: usize,
        affected_rows: u64,
    },
    Database {
        row_index: usize,
        message: String,
    },
}

impl ResultPanel {
    fn pending_row_mutations(
        &self,
        result: &QueryResult,
        identity: &RowIdentity,
        table_ref: &str,
        driver: DriverKind,
    ) -> Result<Vec<PendingRowMutation>, String> {
        let mut by_row: BTreeMap<usize, Vec<PendingEditRequest>> = BTreeMap::new();
        for (&(ri, ci), pending) in &self.pending_cell_edits {
            let key = (ri, ci);
            if result.rows.get(ri).is_none() {
                return Err(format!("第 {} 行已不在当前结果中，请重新查询", ri + 1));
            }
            let Some(column) = result.columns.get(ci) else {
                return Err("待提交修改的列已不在当前结果中，请重新查询".to_string());
            };
            by_row.entry(ri).or_default().push(PendingEditRequest {
                key,
                column: column.clone(),
                value: pending.current.clone(),
            });
        }

        let mut mutations = Vec::with_capacity(by_row.len());
        for (row_index, edits) in by_row {
            let row = result
                .rows
                .get(row_index)
                .ok_or_else(|| format!("第 {} 行已不在当前结果中，请重新查询", row_index + 1))?;
            let identity_row =
                self.row_with_original_identity_values(result, row_index, row, identity);
            let assignments = edits
                .iter()
                .map(|edit| (edit.column.clone(), edit.value.clone()))
                .collect::<Vec<_>>();
            let sql = build_update_sql(
                result,
                &identity_row,
                identity,
                table_ref,
                &assignments,
                driver,
            )
            .map_err(cell_update_sql_error_message)?;
            mutations.push(PendingRowMutation {
                row_index,
                edits,
                query: Query::new(sql),
            });
        }
        Ok(mutations)
    }

    pub(crate) fn commit_pending_cell_edits_async(&mut self, cx: &mut Context<Self>) -> bool {
        if self.pending_cell_edits.is_empty() {
            return false;
        }
        let Some(identity) = self.guard_modify("提交修改", cx) else {
            return false;
        };
        let Some((svc, conn, transaction)) = self.dml_conn("提交修改", cx) else {
            return false;
        };
        let ResultState::Ok(result) = &self.state else {
            return false;
        };
        let Some(table_ref) = self.current_table_ref() else {
            self.pending_notification = Some(
                Notification::error("无法识别目标表，请从表树打开单表后再提交").autohide(true),
            );
            cx.notify();
            return false;
        };
        let mutations = match self.pending_row_mutations(result, &identity, &table_ref, conn.driver)
        {
            Ok(mutations) if !mutations.is_empty() => mutations,
            Ok(_) => return false,
            Err(message) => {
                self.pending_notification = Some(Notification::error(message).autohide(false));
                cx.notify();
                return false;
            }
        };
        for mutation in &mutations {
            if !self.guard_generated_query(&mutation.query, cx) {
                return false;
            }
        }

        let pending_edit_count = self.pending_cell_edits.len();
        let result_revision = self.result_revision;
        let strategy = format!("按{}", identity.label);
        let commit_hint = auto_commit_hint(transaction.as_ref());
        self.dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut succeeded = Vec::new();
            let mut anomalous_mutation = None;
            let mut successful_rows = 0usize;
            let mut affected_rows = 0u64;
            let mut failure = None;
            for mutation in mutations {
                let row_index = mutation.row_index;
                match Self::execute_mutation(&svc, &conn, transaction.as_ref(), &mutation.query)
                    .await
                {
                    Ok(query_result) if query_result.affected_rows > 0 => {
                        successful_rows += 1;
                        affected_rows = affected_rows.saturating_add(query_result.affected_rows);
                        let anomalous = query_result.affected_rows > 1;
                        if anomalous {
                            anomalous_mutation = Some(mutation);
                            failure = Some(PendingBatchFailure::Anomalous {
                                row_index,
                                affected_rows: query_result.affected_rows,
                            });
                            break;
                        }
                        succeeded.push(mutation);
                    }
                    Ok(_) => {
                        failure = Some(PendingBatchFailure::NotMatched { row_index });
                        break;
                    }
                    Err(error) => {
                        error!(
                            operation = "sql_update_batch",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            table = %table_ref,
                            row_index,
                            error = %error,
                            "batch cell update failed"
                        );
                        failure = Some(PendingBatchFailure::Database {
                            row_index,
                            message: error.write_hint("批量更新失败"),
                        });
                        break;
                    }
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                let same_result = this.result_revision == result_revision;
                let mut committed_edits = 0usize;
                let mut result_changed = false;
                for mutation in succeeded {
                    for edit in mutation.edits {
                        let submitted_value_matches = this
                            .pending_cell_edits
                            .get(&edit.key)
                            .is_some_and(|pending| values_equal(&pending.current, &edit.value));
                        if !submitted_value_matches {
                            continue;
                        }
                        if same_result
                            && let ResultState::Ok(result) = &mut this.state
                            && let Some(slot) = Arc::make_mut(result)
                                .rows
                                .get_mut(mutation.row_index)
                                .and_then(|row| row.values.get_mut(edit.key.1))
                            && !values_equal(slot, &edit.value)
                        {
                            *slot = edit.value.clone();
                            result_changed = true;
                        }
                        this.pending_cell_edits.remove(&edit.key);
                        committed_edits += 1;
                    }
                }
                // An UPDATE that affects several rows cannot be reflected safely in one
                // result row. Remove its submitted draft to avoid retrying the same write,
                // and require a fresh query before the user continues editing.
                let mut anomalous_edits = 0usize;
                if let Some(mutation) = anomalous_mutation {
                    for edit in mutation.edits {
                        if this
                            .pending_cell_edits
                            .get(&edit.key)
                            .is_some_and(|pending| values_equal(&pending.current, &edit.value))
                        {
                            this.pending_cell_edits.remove(&edit.key);
                            anomalous_edits += 1;
                        }
                    }
                }
                if result_changed {
                    this.mark_result_changed();
                }
                if affected_rows > 0 {
                    cx.emit(ResultPanelEvent::MutationCompleted);
                }
                let remaining = this.pending_cell_edits.len();
                let remaining_note = if remaining > 0 {
                    format!("；仍保留 {remaining} 项未提交修改，可修正后重试")
                } else {
                    String::new()
                };
                let completed_without_failure = failure.is_none();
                this.pending_notification = Some(match failure {
                    None if same_result && remaining == 0 => Notification::success(format!(
                        "已提交 {committed_edits} 项单元格修改，影响 {affected_rows} 行（{strategy}匹配）{commit_hint}"
                    ))
                    .autohide(true),
                    None => Notification::warning(format!(
                        "已提交 {committed_edits} / {pending_edit_count} 项单元格修改，影响 {affected_rows} 行（{strategy}匹配）{commit_hint}；当前结果已变化，请重新查询核对{remaining_note}"
                    ))
                    .autohide(false),
                    Some(PendingBatchFailure::NotMatched { row_index }) => {
                        Notification::warning(format!(
                            "已提交 {committed_edits} 项修改后，第 {} 行 UPDATE 未匹配到记录（请检查定位键）{remaining_note}",
                            row_index + 1
                        ))
                        .autohide(false)
                    }
                    Some(PendingBatchFailure::Anomalous {
                        row_index,
                        affected_rows: row_affected,
                    }) => Notification::warning(format!(
                        "已提交 {committed_edits} 项修改后，第 {} 行 UPDATE 异常影响 {row_affected} 行（{strategy}），该行 {anomalous_edits} 项草稿已清除，已停止后续提交，请重新查询核对{remaining_note}",
                        row_index + 1
                    ))
                    .autohide(false),
                    Some(PendingBatchFailure::Database { row_index, message }) => {
                        let message = format!(
                            "已提交 {committed_edits} 项修改后，第 {} 行失败：{message}{remaining_note}",
                            row_index + 1
                        );
                        cx.emit(ResultPanelEvent::MutationFailed(message.clone()));
                        Notification::error(message).autohide(false)
                    }
                });
                if successful_rows == 0 && completed_without_failure {
                    this.pending_notification = Some(
                        Notification::warning("没有可提交的单元格修改").autohide(true),
                    );
                }
                cx.notify();
            });
        })
        .detach();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{build_update_sql, values_equal};
    use crate::views::result_panel::RowIdentity;
    use ramag_domain::entities::{DriverKind, QueryResult, Row, Value};

    #[test]
    fn grouped_update_uses_one_original_identity_where_clause() {
        let result = QueryResult {
            columns: vec!["id".into(), "status".into(), "note".into()],
            column_types: vec![],
            rows: vec![],
            affected_rows: 0,
            elapsed_ms: 0,
            warnings: vec![],
            truncated: false,
        };
        let row = Row {
            values: vec![
                Value::Int(7),
                Value::Text("ready".into()),
                Value::Text("old".into()),
            ],
        };
        let identity = RowIdentity {
            columns: vec!["id".into()],
            label: "主键",
        };
        let sql = build_update_sql(
            &result,
            &row,
            &identity,
            "`orders`",
            &[
                ("status".into(), Value::Text("sent".into())),
                ("id".into(), Value::Int(8)),
            ],
            DriverKind::Mysql,
        )
        .expect("grouped UPDATE should fit the query budget");

        assert_eq!(
            sql,
            "UPDATE `orders` SET `status` = 'sent', `id` = 8 WHERE `id` = 7 LIMIT 1;"
        );
    }

    #[test]
    fn grouped_update_rejects_missing_identity_column() {
        let result = QueryResult {
            columns: vec!["status".into()],
            column_types: vec![],
            rows: vec![],
            affected_rows: 0,
            elapsed_ms: 0,
            warnings: vec![],
            truncated: false,
        };
        let row = Row {
            values: vec![Value::Text("ready".into())],
        };
        let identity = RowIdentity {
            columns: vec!["id".into()],
            label: "主键",
        };
        assert!(
            build_update_sql(
                &result,
                &row,
                &identity,
                "`orders`",
                &[("status".into(), Value::Text("sent".into()))],
                DriverKind::Postgres,
            )
            .is_err()
        );
    }

    #[test]
    fn grouped_update_value_comparison_matches_the_staged_value_rule() {
        assert!(values_equal(
            &Value::Text("sent".into()),
            &Value::Text("sent".into())
        ));
        assert!(!values_equal(&Value::Text("sent".into()), &Value::Null));
    }
}
