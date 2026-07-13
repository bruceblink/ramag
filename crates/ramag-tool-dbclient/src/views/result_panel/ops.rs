//! ResultPanel DML：行内编辑触发的 INSERT / UPDATE / DELETE

use std::sync::Arc;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, Query, Value};
use tracing::error;

use super::ResultPanel;
use super::ResultState;
use super::helpers::{
    RowIdentity, build_identity_where, build_new_value, dml_row_limit, escape_new_value_for_old,
};

impl ResultPanel {
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

    /// 行内修改 / 删除的总闸门（按钮已禁用，这里兜底弹框期间条件变化）：
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
    pub(crate) fn delete_preview(&self) -> Option<(usize, String)> {
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
        Some((ri, format!("{col} = {val}")))
    }

    /// 批量删除前的预览：返回 (排序去重后的 indices, "N 行预览" 文案)
    pub(crate) fn delete_preview_multi(&self) -> Option<(Vec<usize>, String)> {
        if self.selected_rows.is_empty() {
            return None;
        }
        let ResultState::Ok(result) = &self.state else {
            return None;
        };
        let mut indices: Vec<usize> = self
            .selected_rows
            .iter()
            .copied()
            .filter(|i| *i < result.rows.len())
            .collect();
        indices.sort();
        indices.dedup();
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
        let summary = format!("将删除 {} 行：{}", indices.len(), samples.join(" / "));
        Some((indices, summary))
    }

    /// 批量执行 DELETE：每行独立 SQL（DELETE ... WHERE ... LIMIT 1），串行 await
    pub(crate) fn execute_delete_rows_async(
        &mut self,
        indices: Vec<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(identity) = self.guard_modify("删除", cx) else {
            return;
        };
        let Some((svc, conn)) = self.dml_conn("删除", cx) else {
            return;
        };
        let ResultState::Ok(result) = &self.state else {
            return;
        };
        let table_ref = match self.current_table_ref() {
            Some(t) => t,
            None => {
                self.pending_notification = Some(
                    Notification::error("无法识别目标表，请从表树打开单表后再删除").autohide(true),
                );
                cx.notify();
                return;
            }
        };

        let strategy = format!("按{}", identity.label);
        let driver = conn.driver;
        let limit_clause = dml_row_limit(driver);
        let mut plans: Vec<(usize, String)> = Vec::with_capacity(indices.len());
        for &ri in &indices {
            let Some(row) = result.rows.get(ri) else {
                continue;
            };
            // 键列缺失（理论上单表 SELECT * 不会发生）：整批拒绝，绝不退化成模糊匹配
            let Some(where_clause) = build_identity_where(result, row, &identity, driver) else {
                self.pending_notification = Some(
                    Notification::error("结果集缺少定位键列，已取消删除；请重新查询该表")
                        .autohide(true),
                );
                cx.notify();
                return;
            };
            let sql = format!("DELETE FROM {table_ref} WHERE {where_clause}{limit_clause};");
            plans.push((ri, sql));
        }
        if plans.is_empty() {
            return;
        }

        self.dml_busy = true;
        cx.spawn(async move |this, cx| {
            let mut deleted: Vec<usize> = Vec::new();
            let mut last_err: Option<ramag_domain::error::DomainError> = None;
            for (ri, sql) in plans {
                let q = Query::new(sql);
                match svc.execute_with_history(&conn, &q).await {
                    Ok(qr) if qr.affected_rows > 0 => deleted.push(ri),
                    Ok(_) => {}
                    Err(e) => {
                        error!(error = %e, "delete row failed (in batch)");
                        last_err = Some(e);
                        break;
                    }
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                if let ResultState::Ok(r) = &mut this.state {
                    let mut to_remove = deleted.clone();
                    to_remove.sort_by(|a, b| b.cmp(a));
                    for ri in to_remove {
                        if ri < r.rows.len() {
                            r.rows.remove(ri);
                        }
                    }
                }
                this.selected_rows.clear();
                this.selected_cell = None;
                this.pending_notification = Some(if let Some(e) = last_err {
                    Notification::error(e.write_hint(&format!("已删除 {} 行后出错", deleted.len())))
                        .autohide(true)
                } else {
                    Notification::success(format!("已删除 {} 行（{strategy}匹配）", deleted.len()))
                        .autohide(true)
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// 新增行弹框确认后调用：异步执行 INSERT，成功后本地 rows.push
    pub(crate) fn apply_insert_async(
        &mut self,
        values: Vec<(String, Value)>,
        cx: &mut Context<Self>,
    ) {
        if values.is_empty() {
            return;
        }
        // 兜底草稿行提交时条件已变化（如手改 SQL / 配置转生产只读）
        if let Some(reason) = self.insert_block_reason() {
            self.pending_notification =
                Some(Notification::warning(format!("无法新增：{reason}")).autohide(true));
            cx.notify();
            return;
        }
        let Some((svc, conn)) = self.dml_conn("新增", cx) else {
            return;
        };
        let table_ref = match self.current_table_ref() {
            Some(t) => t,
            None => {
                self.pending_notification = Some(
                    Notification::error("无法识别目标表，请从表树打开单表后再新增").autohide(true),
                );
                cx.notify();
                return;
            }
        };

        let driver = conn.driver;
        let cols_sql = values
            .iter()
            .map(|(c, _)| driver.quote_identifier(c))
            .collect::<Vec<_>>()
            .join(", ");
        let vals_sql = values
            .iter()
            .map(|(_, v)| v.to_sql_literal_for(driver))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("INSERT INTO {table_ref} ({cols_sql}) VALUES ({vals_sql});");
        let q = Query::new(sql);
        let new_row_values: Option<Vec<Value>> = match &self.state {
            ResultState::Ok(r) => Some(
                r.columns
                    .iter()
                    .map(|c| {
                        values
                            .iter()
                            .find(|(name, _)| name.eq_ignore_ascii_case(c))
                            .map(|(_, v)| v.clone())
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            ),
            _ => None,
        };

        self.dml_busy = true;
        cx.spawn(async move |this, cx| {
            let outcome = svc.execute_with_history(&conn, &q).await;
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                match outcome {
                    Ok(qr) => {
                        if qr.affected_rows == 0 {
                            this.pending_notification = Some(
                                Notification::warning("INSERT 未影响任何行（请检查约束）")
                                    .autohide(true),
                            );
                        } else {
                            if let (ResultState::Ok(r), Some(vs)) =
                                (&mut this.state, new_row_values)
                            {
                                r.rows.push(ramag_domain::entities::Row { values: vs });
                            }
                            this.pending_notification = Some(
                                Notification::success(format!("已新增 {} 行", qr.affected_rows))
                                    .autohide(true),
                            );
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "insert row failed");
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("新增失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 二次确认后真执行 DELETE：异步发到 DB，成功后本地移除该行
    pub(crate) fn execute_delete_row_async(&mut self, ri: usize, cx: &mut Context<Self>) {
        let Some(identity) = self.guard_modify("删除", cx) else {
            return;
        };
        let Some((svc, conn)) = self.dml_conn("删除", cx) else {
            return;
        };
        let ResultState::Ok(result) = &self.state else {
            return;
        };
        let Some(row) = result.rows.get(ri).cloned() else {
            return;
        };

        let table_ref = match self.current_table_ref() {
            Some(t) => t,
            None => {
                self.pending_notification = Some(
                    Notification::error("无法识别目标表，请从表树打开单表后再删除").autohide(true),
                );
                cx.notify();
                return;
            }
        };

        let strategy = format!("按{}", identity.label);
        let Some(where_clause) = build_identity_where(result, &row, &identity, conn.driver) else {
            self.pending_notification = Some(
                Notification::error("结果集缺少定位键列，已取消删除；请重新查询该表")
                    .autohide(true),
            );
            cx.notify();
            return;
        };
        let limit_clause = dml_row_limit(conn.driver);
        let sql = format!("DELETE FROM {table_ref} WHERE {where_clause}{limit_clause};");
        let q = Query::new(sql);

        self.dml_busy = true;
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
                            if let ResultState::Ok(r) = &mut this.state
                                && ri < r.rows.len()
                            {
                                r.rows.remove(ri);
                            }
                            this.selected_cell = None;
                            // affected>1：按定位键仍命中多行属于异常（键失效 / 元数据漂移），
                            // 本地只移除了一行，DB 实际删了多行——数据已不一致，必须显式告警
                            if qr.affected_rows > 1 {
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "注意：本次 DELETE 影响了 {} 行（{strategy}），定位键可能已失效，请重新查询核对",
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
                        error!(error = %e, "delete row failed");
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("删除失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 单元格编辑弹框「确认修改」：异步执行 UPDATE，成功后同步本地 cell
    pub(crate) fn apply_cell_update_async(
        &mut self,
        ri: usize,
        ci: usize,
        new_text: String,
        cx: &mut Context<Self>,
    ) {
        let Some(identity) = self.guard_modify("修改", cx) else {
            return;
        };
        let Some((svc, conn)) = self.dml_conn("修改", cx) else {
            return;
        };
        let ResultState::Ok(result) = &self.state else {
            return;
        };
        let Some(row) = result.rows.get(ri).cloned() else {
            return;
        };
        let Some(col_name) = result.columns.get(ci).cloned() else {
            return;
        };
        let Some(cell_val) = row.values.get(ci).cloned() else {
            return;
        };

        let table_ref = match self.current_table_ref() {
            Some(t) => t,
            None => {
                self.pending_notification = Some(
                    Notification::error("无法识别目标表，请从表树打开单表后再编辑").autohide(true),
                );
                cx.notify();
                return;
            }
        };

        let strategy = format!("按{}", identity.label);
        let driver = conn.driver;
        let Some(where_clause) = build_identity_where(result, &row, &identity, driver) else {
            self.pending_notification = Some(
                Notification::error("结果集缺少定位键列，已取消修改；请重新查询该表")
                    .autohide(true),
            );
            cx.notify();
            return;
        };
        let new_literal = escape_new_value_for_old(&cell_val, &new_text, driver);
        let limit_clause = dml_row_limit(driver);
        let sql = format!(
            "UPDATE {table_ref} SET {} = {new_literal} WHERE {where_clause}{limit_clause};",
            driver.quote_identifier(&col_name),
        );
        let new_cell_val = build_new_value(&cell_val, &new_text);
        let q = Query::new(sql);

        self.dml_busy = true;
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
                            if let ResultState::Ok(r) = &mut this.state
                                && let Some(row) = r.rows.get_mut(ri)
                                && let Some(slot) = row.values.get_mut(ci)
                            {
                                *slot = new_cell_val;
                            }
                            // affected>1：按定位键仍命中多行属于异常（键失效 / 元数据漂移），
                            // 意味着可能误改了其它行，必须显式告警而非当成功
                            if qr.affected_rows > 1 {
                                this.pending_notification = Some(
                                    Notification::warning(format!(
                                        "注意：本次 UPDATE 影响了 {} 行（{strategy}），定位键可能已失效，请核对数据",
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
                        error!(error = %e, "apply cell update failed");
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("更新失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
