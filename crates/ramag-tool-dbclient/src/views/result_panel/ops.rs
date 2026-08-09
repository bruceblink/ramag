//! ResultPanel DML：行内编辑触发的 INSERT / UPDATE / DELETE

mod delete;
use std::sync::Arc;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, MAX_SQL_QUERY_BYTES, Query, Value};
use tracing::error;

use super::ResultPanel;
use super::ResultState;
use super::helpers::{
    IdentityWhereError, MAX_BATCH_DELETE_ROWS, MAX_BATCH_DELETE_SQL_BYTES, RowIdentity,
    batch_delete_notice, build_identity_where, build_new_value, dml_row_limit,
    reserve_batch_delete_sql_bytes,
};

fn identity_where_error_message(error: IdentityWhereError) -> &'static str {
    match error {
        IdentityWhereError::MissingColumn => "结果集缺少定位键列，已取消操作；请重新查询该表",
        IdentityWhereError::TooLarge => {
            "行定位键生成的 SQL 超过 32 MiB 安全上限，已取消操作；请用手写 SQL 处理"
        }
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

    /// DML 前置守卫：取连接服务 + 连接配置，缺任一即弹 toast 返回 None。
    /// `action` 用于提示文案（删除 / 新增 / 修改）。
    fn dml_conn(
        &mut self,
        action: &str,
        cx: &mut Context<Self>,
    ) -> Option<(Arc<ConnectionService>, ConnectionConfig)> {
        // 防重入：上一 DML 未回包前拒绝新提交，避免连点确认重复执行
        if self.dml_busy {
            self.pending_notification = Some(
                Notification::warning(format!("上一操作尚未完成，请稍候再{action}")).autohide(true),
            );
            cx.notify();
            return None;
        }
        let (Some(svc), Some(conn)) = (self.service.clone(), self.connection.clone()) else {
            self.pending_notification =
                Some(Notification::warning(format!("当前未注入连接，无法{action}")).autohide(true));
            cx.notify();
            return None;
        };
        Some((svc, conn))
    }

    /// 行内修改 / 删除的总闸门（按钮已禁用，兜底处理弹框期间的条件变化）：
    /// 过闸返回行定位键，未过弹 toast 返回 None
    fn guard_modify(&mut self, action: &str, cx: &mut Context<Self>) -> Option<RowIdentity> {
        if let Some(reason) = self.modify_block_reason() {
            self.pending_notification =
                Some(Notification::warning(format!("无法{action}：{reason}")).autohide(true));
            cx.notify();
            return None;
        }
        self.row_identity.clone()
    }

    /// 删除前的预览数据：(row_idx, "列=值" 简短文案)；调用方拿去给 confirm dialog 用
    /// 优先用行定位键第一列做预览，无键用第一列
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

    /// 批量删除前的预览：返回 (排序去重后的 indices, "N 行预览" 文案)
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

    /// 二次确认后真执行 DELETE：异步发到 DB，成功后本地移除该行
    pub(crate) fn execute_delete_row_async(&mut self, ri: usize, cx: &mut Context<Self>) -> bool {
        let Some(identity) = self.guard_modify("删除", cx) else {
            return false;
        };
        let Some((svc, conn)) = self.dml_conn("删除", cx) else {
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

        let result_revision = self.result_revision;
        self.dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = svc.execute_with_history(&conn, &q).await;
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                match outcome {
                    Ok(qr) => {
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
                            // affected>1：按定位键仍命中多行属于异常（键失效 / 元数据漂移），
                            // 本地只移除了一行，DB 实际删了多行——数据已不一致，必须显式告警
                            if qr.affected_rows > 1 {
                                let stale_note = if same_result {
                                    ""
                                } else {
                                    "；当前结果已变化"
                                };
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "注意：本次 DELETE 影响了 {} 行（{strategy}），定位键可能已失效{stale_note}，请重新查询核对",
                                        qr.affected_rows,
                                    ))
                                    .autohide(false),
                                );
                            } else if !same_result {
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "已删除 {} 行（{strategy}匹配）；当前结果已变化，请重新查询核对",
                                        qr.affected_rows
                                    ))
                                    .autohide(false),
                                );
                            } else {
                                this.pending_notification = Some(
                                    Notification::success(format!(
                                        "已删除 {} 行（{strategy}匹配）",
                                        qr.affected_rows
                                    ))
                                    .autohide(true),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!(
                            operation = "sql_delete",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            table = %table_ref,
                            error = %e,
                            "delete row failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("删除失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// 单元格编辑弹框「确认修改」：异步执行 UPDATE，成功后同步本地 cell
    pub(crate) fn apply_cell_update_async(
        &mut self,
        ri: usize,
        ci: usize,
        new_text: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(identity) = self.guard_modify("修改", cx) else {
            return false;
        };
        let Some((svc, conn)) = self.dml_conn("修改", cx) else {
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
        let Some(cell_val) = row.values.get(ci).cloned() else {
            return false;
        };

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

        let strategy = format!("按{}", identity.label);
        let driver = conn.driver;
        let where_clause = match build_identity_where(result, &row, &identity, driver) {
            Ok(where_clause) => where_clause,
            Err(error) => {
                self.pending_notification =
                    Some(Notification::error(identity_where_error_message(error)).autohide(false));
                cx.notify();
                return false;
            }
        };
        let new_cell_val = build_new_value(&cell_val, &new_text);
        let limit_clause = dml_row_limit(driver);
        let column = driver.quote_identifier(&col_name);
        let fixed_sql_bytes = "UPDATE  SET  =  WHERE ;"
            .len()
            .saturating_add(table_ref.len())
            .saturating_add(column.len())
            .saturating_add(where_clause.len())
            .saturating_add(limit_clause.len());
        let remaining = MAX_SQL_QUERY_BYTES.saturating_sub(fixed_sql_bytes);
        if new_cell_val
            .bounded_sql_literal_len_for(driver, remaining)
            .is_none()
        {
            self.pending_notification = Some(
                Notification::error(format!(
                    "UPDATE 生成的 SQL 超过 {} MiB 安全上限，请缩短单元格内容",
                    MAX_SQL_QUERY_BYTES / 1024 / 1024
                ))
                .autohide(false),
            );
            cx.notify();
            return false;
        }
        let new_literal = new_cell_val.to_sql_literal_for(driver);
        let sql = format!(
            "UPDATE {table_ref} SET {column} = {new_literal} WHERE {where_clause}{limit_clause};",
        );
        let q = Query::new(sql);
        if !self.guard_generated_query(&q, cx) {
            return false;
        }

        let result_revision = self.result_revision;
        self.dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = svc.execute_with_history(&conn, &q).await;
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                match outcome {
                    Ok(qr) => {
                        if qr.affected_rows == 0 {
                            this.pending_notification = Some(
                                Notification::warning("UPDATE 未匹配到记录（请检查主键）")
                                    .autohide(true),
                            );
                        } else {
                            let same_result = this.result_revision == result_revision;
                            let mut result_changed = false;
                            if same_result
                                && let ResultState::Ok(r) = &mut this.state
                            {
                                let r = Arc::make_mut(r);
                                if let Some(row) = r.rows.get_mut(ri)
                                    && let Some(slot) = row.values.get_mut(ci)
                                {
                                    *slot = new_cell_val;
                                    result_changed = true;
                                }
                            }
                            if result_changed {
                                this.mark_result_changed();
                            }
                            // affected>1：按定位键仍命中多行属于异常（键失效 / 元数据漂移），
                            // 意味着可能误改了其它行，必须显式告警而非当成功
                            if qr.affected_rows > 1 {
                                let stale_note = if same_result {
                                    ""
                                } else {
                                    "；当前结果已变化"
                                };
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "注意：本次 UPDATE 影响了 {} 行（{strategy}），定位键可能已失效{stale_note}，请重新查询核对",
                                        qr.affected_rows,
                                    ))
                                    .autohide(false),
                                );
                            } else if !same_result {
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "已更新 {} 行（{strategy}匹配）；当前结果已变化，请重新查询核对",
                                        qr.affected_rows
                                    ))
                                    .autohide(false),
                                );
                            } else {
                                this.pending_notification = Some(
                                    Notification::success(format!(
                                        "已更新 {} 行（{strategy}匹配）",
                                        qr.affected_rows
                                    ))
                                    .autohide(true),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!(
                            operation = "sql_update",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            table = %table_ref,
                            column = %col_name,
                            error = %e,
                            "apply cell update failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("更新失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }
}
