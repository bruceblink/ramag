use super::*;

impl ResultPanel {
    /// 批量删除逐行执行。
    pub(crate) fn execute_delete_rows_async(
        &mut self,
        indices: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.guard_batch_delete_count(indices.len(), cx) {
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
        let driver = conn.driver;
        let limit_clause = dml_row_limit(driver);
        let mut plans: Vec<(usize, Query)> = Vec::with_capacity(indices.len());
        let mut total_sql_bytes = 0usize;
        for &ri in &indices {
            let Some(row) = result.rows.get(ri) else {
                continue;
            };
            // 键列缺失（理论上单表 SELECT * 不会发生）：整批拒绝，绝不退化成模糊匹配
            let where_clause = match build_identity_where(result, row, &identity, driver) {
                Ok(where_clause) => where_clause,
                Err(error) => {
                    self.pending_notification = Some(
                        Notification::error(identity_where_error_message(error)).autohide(false),
                    );
                    cx.notify();
                    return false;
                }
            };
            let sql = format!("DELETE FROM {table_ref} WHERE {where_clause}{limit_clause};");
            let Some(next_total) = reserve_batch_delete_sql_bytes(total_sql_bytes, sql.len())
            else {
                self.pending_notification = Some(
                    Notification::error(format!(
                        "批量 DELETE 语句合计超过 {} MiB 安全上限，请减少勾选行数",
                        MAX_BATCH_DELETE_SQL_BYTES / 1024 / 1024
                    ))
                    .autohide(false),
                );
                cx.notify();
                return false;
            };
            let query = Query::new(sql);
            if let Err(error) = query.validate() {
                self.pending_notification =
                    Some(Notification::error(error.to_string()).autohide(false));
                cx.notify();
                return false;
            }
            total_sql_bytes = next_total;
            plans.push((ri, query));
        }
        if plans.is_empty() {
            return false;
        }

        let result_revision = self.result_revision;
        self.dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut deleted: Vec<usize> = Vec::new();
            let mut affected_rows = 0u64;
            let mut not_matched = 0usize;
            let mut anomalous_affected = None;
            let mut last_err: Option<ramag_domain::error::DomainError> = None;
            for (ri, query) in plans {
                match ResultPanel::execute_mutation(&svc, &conn, transaction.as_ref(), &query).await
                {
                    Ok(qr) if qr.affected_rows > 0 => {
                        affected_rows = affected_rows.saturating_add(qr.affected_rows);
                        deleted.push(ri);
                        if qr.affected_rows > 1 {
                            anomalous_affected = Some(qr.affected_rows);
                            break;
                        }
                    }
                    Ok(_) => not_matched += 1,
                    Err(e) => {
                        error!(
                            operation = "sql_delete",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            table = %table_ref,
                            mode = "batch",
                            error = %e,
                            "delete row failed"
                        );
                        last_err = Some(e);
                        break;
                    }
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                if affected_rows > 0 {
                    cx.emit(crate::views::result_panel::ResultPanelEvent::MutationCompleted);
                }
                let same_result = this.result_revision == result_revision;
                let mut result_changed = false;
                if same_result {
                    if let ResultState::Ok(r) = &mut this.state {
                        let r = Arc::make_mut(r);
                        let before = r.rows.len();
                        // 删除计划源自 BTreeSet，成功项保持升序；逆序移除即可避免索引位移。
                        for &ri in deleted.iter().rev() {
                            if ri < r.rows.len() {
                                r.rows.remove(ri);
                            }
                        }
                        result_changed = r.rows.len() != before;
                    }
                    this.clear_selected_rows();
                    this.selected_cell = None;
                }
                if result_changed {
                    this.mark_result_changed();
                }
                this.pending_notification = Some(if let Some(e) = last_err {
                    let stale_note = if same_result {
                        ""
                    } else {
                        "；当前结果已变化"
                    };
                    let message = e.write_hint(&format!(
                        "已影响 {affected_rows} 行、{not_matched} 行未匹配后出错{stale_note}"
                    ));
                    cx.emit(
                        crate::views::result_panel::ResultPanelEvent::MutationFailed(
                            message.clone(),
                        ),
                    );
                    Notification::error(message).autohide(true)
                } else {
                    let notice = batch_delete_notice(
                        deleted.len(),
                        affected_rows,
                        not_matched,
                        anomalous_affected,
                        &strategy,
                        same_result,
                    );
                    if notice.persistent {
                        Notification::warning(notice.message).autohide(false)
                    } else {
                        Notification::success(notice.message).autohide(true)
                    }
                });
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// 新增行后异步插入并更新本地结果。
    pub(crate) fn apply_insert_async(
        &mut self,
        values: Vec<(String, Value)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if values.is_empty() {
            return false;
        }
        // 草稿期间的条件可能已变化。
        if let Some(reason) = self.insert_block_reason() {
            self.pending_notification =
                Some(Notification::warning(format!("无法新增：{reason}")).autohide(true));
            cx.notify();
            return false;
        }
        let Some((svc, conn, transaction)) = self.dml_conn("新增", cx) else {
            return false;
        };
        let table_ref = match self.current_table_ref() {
            Some(t) => t,
            None => {
                self.pending_notification = Some(
                    Notification::error("无法识别目标表，请从表树打开单表后再新增").autohide(true),
                );
                cx.notify();
                return false;
            }
        };

        let driver = conn.driver;
        let cols_sql = values
            .iter()
            .map(|(c, _)| driver.quote_identifier(c))
            .collect::<Vec<_>>()
            .join(", ");
        let mut estimated_sql_bytes = "INSERT INTO  () VALUES ();"
            .len()
            .saturating_add(table_ref.len())
            .saturating_add(cols_sql.len());
        for (index, (_, value)) in values.iter().enumerate() {
            estimated_sql_bytes = estimated_sql_bytes.saturating_add(usize::from(index > 0) * 2);
            let remaining = MAX_SQL_QUERY_BYTES.saturating_sub(estimated_sql_bytes);
            let Some(literal_bytes) = value.bounded_sql_literal_len_for(driver, remaining) else {
                self.pending_notification = Some(
                    Notification::error(format!(
                        "INSERT 生成的 SQL 超过 {} MiB 安全上限，请减少列或缩短输入",
                        MAX_SQL_QUERY_BYTES / 1024 / 1024
                    ))
                    .autohide(false),
                );
                cx.notify();
                return false;
            };
            estimated_sql_bytes = estimated_sql_bytes.saturating_add(literal_bytes);
        }
        let vals_sql = values
            .iter()
            .map(|(_, v)| v.to_sql_literal_for(driver))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("INSERT INTO {table_ref} ({cols_sql}) VALUES ({vals_sql});");
        let q = Query::new(sql);
        if !self.guard_generated_query(&q, cx) {
            return false;
        }
        let result_revision = self.result_revision;
        self.dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome =
                ResultPanel::execute_mutation(&svc, &conn, transaction.as_ref(), &q).await;
            let _ = this.update(cx, |this, cx| {
                this.dml_busy = false;
                match outcome {
                    Ok(qr) => {
                        cx.emit(crate::views::result_panel::ResultPanelEvent::MutationCompleted);
                        if qr.affected_rows == 0 {
                            this.pending_notification = Some(
                                Notification::warning("INSERT 未影响任何行（请检查约束）")
                                    .autohide(true),
                            );
                        } else {
                            let same_result = this.result_revision == result_revision;
                            this.pending_notification = Some(if same_result {
                                Notification::success(format!(
                                    "已新增 {} 行；请重新查询查看数据库默认值和生成字段",
                                    qr.affected_rows
                                ))
                                .autohide(true)
                            } else {
                                Notification::warning(format!(
                                    "已新增 {} 行；当前结果已变化，请重新查询核对",
                                    qr.affected_rows
                                ))
                                .autohide(false)
                            });
                        }
                    }
                    Err(e) => {
                        error!(
                            operation = "sql_insert",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            table = %table_ref,
                            error = %e,
                            "insert row failed"
                        );
                        let message = e.write_hint("新增失败");
                        cx.emit(
                            crate::views::result_panel::ResultPanelEvent::MutationFailed(
                                message.clone(),
                            ),
                        );
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
}
