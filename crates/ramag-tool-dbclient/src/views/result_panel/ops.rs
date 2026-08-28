//! 结果集行内写操作。

#[path = "cell_updates.rs"]
mod cell_updates;
mod delete;
use std::sync::Arc;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::ConnectionService;
use ramag_domain::entities::{
    ConnectionConfig, MAX_SQL_QUERY_BYTES, Query, QueryResult, Row, TransactionId, Value,
};
use tracing::error;

use super::ResultPanel;
use super::ResultState;
use super::helpers::{
    IdentityWhereError, MAX_BATCH_DELETE_ROWS, MAX_BATCH_DELETE_SQL_BYTES, MAX_PENDING_CELL_EDITS,
    PendingCellEdit, RowIdentity, batch_delete_notice, build_identity_where, build_new_value,
    dml_row_limit, reserve_batch_delete_sql_bytes, values_equal,
};
use cell_updates::{build_update_sql, cell_update_sql_error_message};

fn identity_where_error_message(error: IdentityWhereError) -> &'static str {
    match error {
        IdentityWhereError::MissingColumn => "结果集缺少定位键列，已取消操作；请重新查询该表",
        IdentityWhereError::TooLarge => {
            "行定位键生成的 SQL 超过 32 MiB 安全上限，已取消操作；请用手写 SQL 处理"
        }
    }
}

fn auto_commit_hint(transaction: Option<&TransactionId>) -> &'static str {
    if transaction.is_none() {
        "（已自动提交）"
    } else {
        ""
    }
}

impl ResultPanel {
    fn guard_generated_query(&mut self, query: &Query, cx: &mut Context<Self>) -> bool {
        if let Err(error) = query.validate() {
            self.pending_notification =
                Some(Notification::error(error.to_string()).autohide(false));
            cx.notify();
            return false;
        }
        true
    }

    pub(crate) fn guard_batch_delete_count(
        &mut self,
        count: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        if count <= MAX_BATCH_DELETE_ROWS {
            return true;
        }
        self.pending_notification = Some(
            Notification::warning(format!(
                "单次批量删除最多 {MAX_BATCH_DELETE_ROWS} 行，请减少勾选后分批执行"
            ))
            .autohide(false),
        );
        cx.notify();
        false
    }

    /// 返回可执行 DML 的连接。
    fn dml_conn(
        &mut self,
        action: &str,
        cx: &mut Context<Self>,
    ) -> Option<(
        Arc<ConnectionService>,
        ConnectionConfig,
        Option<TransactionId>,
    )> {
        if self.dml_busy {
            self.pending_notification = Some(
                Notification::warning(format!("上一操作尚未完成，请稍候再{action}")).autohide(true),
            );
            cx.notify();
            return None;
        }
        if self.transaction_busy {
            self.pending_notification =
                Some(Notification::info(format!("事务正在切换，暂时无法{action}")).autohide(true));
            cx.notify();
            return None;
        }
        let (Some(svc), Some(conn)) = (self.service.clone(), self.connection.clone()) else {
            self.pending_notification =
                Some(Notification::warning(format!("当前未注入连接，无法{action}")).autohide(true));
            cx.notify();
            return None;
        };
        Some((svc, conn, self.transaction_id.clone()))
    }

    /// Routes generated mutations through the active manual transaction when present.
    pub(super) async fn execute_mutation(
        service: &ConnectionService,
        connection: &ConnectionConfig,
        transaction: Option<&TransactionId>,
        query: &Query,
    ) -> ramag_domain::error::Result<QueryResult> {
        match transaction {
            Some(transaction) => {
                service
                    .execute_in_transaction(connection, transaction, query)
                    .await
            }
            None => service.execute_with_history(connection, query).await,
        }
    }

    /// 检查行内写操作。
    fn guard_modify(&mut self, action: &str, cx: &mut Context<Self>) -> Option<RowIdentity> {
        if let Some(reason) = self.modify_block_reason() {
            self.pending_notification =
                Some(Notification::warning(format!("无法{action}：{reason}")).autohide(true));
            cx.notify();
            return None;
        }
        self.row_identity.clone()
    }

    pub(crate) fn delete_preview(&self, cx: &gpui::App) -> Option<(usize, String)> {
        let (ri, _) = self.selected_cell?;
        let ResultState::Ok(result) = &self.state else {
            return None;
        };
        let row = result.rows.get(ri)?;
        let idx = self.preview_col_idx(result);
        let col = result.columns.get(idx)?.clone();
        let val = row
            .values
            .get(idx)
            .map(|v| v.display_preview(60))
            .unwrap_or_default();
        let visible = crate::views::result_table::cached_display_view(self, result, cx)
            .is_none_or(|view| view.display_indices.contains(&ri));
        let hidden_note = if visible {
            ""
        } else {
            "（该行当前被筛选隐藏）"
        };
        Some((ri, format!("{col} = {val}{hidden_note}")))
    }

    pub(crate) fn delete_preview_multi(&self, cx: &gpui::App) -> Option<(Vec<usize>, String)> {
        if self.selected_rows.is_empty() {
            return None;
        }
        let ResultState::Ok(result) = &self.state else {
            return None;
        };
        let indices: Vec<usize> = self
            .selected_rows
            .iter()
            .copied()
            .filter(|i| *i < result.rows.len())
            .collect();
        if indices.is_empty() {
            return None;
        }
        let pk_or_first = self.preview_col_idx(result);
        let preview_col = result.columns.get(pk_or_first).cloned().unwrap_or_default();
        let mut samples: Vec<String> = indices
            .iter()
            .take(3)
            .filter_map(|&ri| {
                let row = result.rows.get(ri)?;
                let val = row
                    .values
                    .get(pk_or_first)
                    .map(|v| v.display_preview(40))
                    .unwrap_or_default();
                Some(format!("{preview_col} = {val}"))
            })
            .collect();
        if indices.len() > 3 {
            samples.push(format!("…还有 {} 行", indices.len() - 3));
        }
        let hidden = crate::views::result_table::cached_display_view(self, result, cx)
            .map(|view| {
                let visible = view
                    .display_indices
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>();
                indices.iter().filter(|ri| !visible.contains(ri)).count()
            })
            .unwrap_or(0);
        let hidden_note = if hidden > 0 {
            format!("（其中 {hidden} 行当前被筛选隐藏）")
        } else {
            String::new()
        };
        let summary = format!(
            "将删除 {} 行{hidden_note}：{}",
            indices.len(),
            samples.join(" / ")
        );
        Some((indices, summary))
    }

    pub(crate) fn execute_delete_row_async(&mut self, ri: usize, cx: &mut Context<Self>) -> bool {
        if !self.guard_no_pending_cell_edits("删除", cx) {
            return false;
        }
        let Some(identity) = self.guard_modify("删除", cx) else {
            return false;
        };
        let Some((svc, conn, transaction)) = self.dml_conn("删除", cx) else {
            return false;
        };
        let ResultState::Ok(result) = &self.state else {
            return false;
        };
        let Some(row) = result.rows.get(ri).cloned() else {
            return false;
        };

        let table_ref = match self.current_table_ref() {
            Some(t) => t,
            None => {
                self.pending_notification = Some(
                    Notification::error("无法识别目标表，请从表树打开单表后再删除").autohide(true),
                );
                cx.notify();
                return false;
            }
        };

        let strategy = format!("按{}", identity.label);
        let where_clause = match build_identity_where(result, &row, &identity, conn.driver) {
            Ok(where_clause) => where_clause,
            Err(error) => {
                self.pending_notification =
                    Some(Notification::error(identity_where_error_message(error)).autohide(false));
                cx.notify();
                return false;
            }
        };
        let limit_clause = dml_row_limit(conn.driver);
        let sql = format!("DELETE FROM {table_ref} WHERE {where_clause}{limit_clause};");
        let q = Query::new(sql);
        if !self.guard_generated_query(&q, cx) {
            return false;
        }
        let commit_hint = auto_commit_hint(transaction.as_ref());

        let result_revision = self.result_revision;
        self.dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = Self::execute_mutation(&svc, &conn, transaction.as_ref(), &q).await;
            if let Err(error) = &outcome {
                error!(
                    operation = "sql_delete",
                    connection_id = %conn.id,
                    driver = ?conn.driver,
                    table = %table_ref,
                    error = %error,
                    "delete row failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                match outcome {
                    Ok(qr) => {
                        cx.emit(super::ResultPanelEvent::MutationCompleted);
                        if qr.affected_rows == 0 {
                            this.pending_notification = Some(
                                Notification::warning(
                                    "DELETE 未匹配到记录（请检查主键或行已被改动）",
                                )
                                .autohide(true),
                            );
                        } else {
                            let same_result = this.result_revision == result_revision;
                            let mut result_changed = false;
                            if same_result
                                && let ResultState::Ok(r) = &mut this.state
                            {
                                let r = Arc::make_mut(r);
                                if ri < r.rows.len() {
                                    r.rows.remove(ri);
                                    result_changed = true;
                                }
                            }
                            if result_changed {
                                this.selected_cell = None;
                                this.mark_result_changed();
                            }
                            // 多行命中时本地结果可能失效。
                            if qr.affected_rows > 1 {
                                let stale_note = if same_result {
                                    ""
                                } else {
                                    "；当前结果已变化"
                                };
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "注意：本次 DELETE 影响了 {} 行（{strategy}），定位键可能已失效{commit_hint}{stale_note}，请重新查询核对",
                                        qr.affected_rows,
                                    ))
                                    .autohide(false),
                                );
                            } else if !same_result {
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "已删除 {} 行（{strategy}匹配）{commit_hint}；当前结果已变化，请重新查询核对",
                                        qr.affected_rows,
                                    ))
                                    .autohide(false),
                                );
                            } else {
                                this.pending_notification = Some(
                                    Notification::success(format!(
                                        "已删除 {} 行（{strategy}匹配）{commit_hint}",
                                        qr.affected_rows,
                                    ))
                                    .autohide(true),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let message = e.write_hint("删除失败");
                        cx.emit(super::ResultPanelEvent::MutationFailed(message.clone()));
                        this.pending_notification =
                            Some(Notification::error(message).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// Stages one cell locally after building the same safe UPDATE that batch submission will use.
    pub(crate) fn stage_cell_update(
        &mut self,
        ri: usize,
        ci: usize,
        new_text: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(identity) = self.guard_modify("修改", cx) else {
            return false;
        };
        let Some((_, conn, _)) = self.dml_conn("修改", cx) else {
            return false;
        };
        let ResultState::Ok(result) = &self.state else {
            return false;
        };
        let Some(row) = result.rows.get(ri).cloned() else {
            return false;
        };
        let Some(col_name) = result.columns.get(ci).cloned() else {
            return false;
        };
        let Some(cell_val) = self.cell_value(ri, ci).cloned() else {
            return false;
        };
        let driver = conn.driver;
        let key = (ri, ci);
        let original = self
            .pending_cell_edits
            .get(&key)
            .map(|pending| pending.original.clone())
            .unwrap_or_else(|| cell_val.clone());
        let new_cell_val = build_new_value(&original, &new_text);
        if values_equal(&cell_val, &new_cell_val) {
            return true;
        }
        if values_equal(&original, &new_cell_val) {
            self.pending_cell_edits.remove(&key);
            cx.notify();
            return true;
        }
        if !self.pending_cell_edits.contains_key(&key)
            && self.pending_cell_edits.len() >= MAX_PENDING_CELL_EDITS
        {
            self.pending_notification = Some(
                Notification::warning(format!(
                    "单次最多保留 {MAX_PENDING_CELL_EDITS} 项未提交单元格修改，请先提交或撤销"
                ))
                .autohide(false),
            );
            cx.notify();
            return false;
        }

        let table_ref = match self.current_table_ref() {
            Some(t) => t,
            None => {
                self.pending_notification = Some(
                    Notification::error("无法识别目标表，请从表树打开单表后再编辑").autohide(true),
                );
                cx.notify();
                return false;
            }
        };
        let identity_row = self.row_with_original_identity_values(result, ri, &row, &identity);
        let sql = match build_update_sql(
            result,
            &identity_row,
            &identity,
            &table_ref,
            &[(col_name.clone(), new_cell_val.clone())],
            driver,
        ) {
            Ok(sql) => sql,
            Err(error) => {
                self.pending_notification =
                    Some(Notification::error(cell_update_sql_error_message(error)).autohide(false));
                cx.notify();
                return false;
            }
        };
        let q = Query::new(sql);
        if !self.guard_generated_query(&q, cx) {
            return false;
        }

        self.pending_cell_edits.insert(
            key,
            PendingCellEdit {
                original,
                current: new_cell_val,
            },
        );
        cx.notify();
        true
    }

    fn row_with_original_identity_values(
        &self,
        result: &QueryResult,
        ri: usize,
        row: &Row,
        identity: &RowIdentity,
    ) -> Row {
        let mut identity_row = row.clone();
        for ((pending_ri, pending_ci), pending) in &self.pending_cell_edits {
            if *pending_ri != ri {
                continue;
            }
            let Some(column) = result.columns.get(*pending_ci) else {
                continue;
            };
            if identity
                .columns
                .iter()
                .any(|identity_column| identity_column.eq_ignore_ascii_case(column))
                && let Some(slot) = identity_row.values.get_mut(*pending_ci)
            {
                *slot = pending.original.clone();
            }
        }
        identity_row
    }
}
