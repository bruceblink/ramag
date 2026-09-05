//! 表树 DDL 执行与完成后的状态刷新。

use std::time::Instant;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_domain::entities::{DriverKind, Query};

use super::{
    TableTreePanel,
    ddl::{clear_invalidated_table_state, success_message},
};

pub(super) enum AfterDdl {
    None,
    ReloadSchema {
        schema: String,
        invalidated_table: String,
    },
    FullRefresh {
        invalidated_schema: String,
    },
}

pub(super) struct TableDdlNotification;

type DdlCompletion =
    Box<dyn FnOnce(bool, &mut TableTreePanel, &mut Context<TableTreePanel>) + 'static>;

impl TableTreePanel {
    pub(super) fn exec_ddl(
        &mut self,
        sql: String,
        success_msg: String,
        after: AfterDdl,
        cx: &mut Context<Self>,
    ) -> bool {
        self.exec_ddl_with_completion(sql, success_msg, after, None, cx)
    }

    pub(super) fn exec_ddl_with_completion(
        &mut self,
        sql: String,
        success_msg: String,
        after: AfterDdl,
        completion: Option<DdlCompletion>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(conn) = self.connection.clone() else {
            return false;
        };
        let Some(mutation_token) = self.ddl_gate.begin() else {
            self.pending_notification =
                Some(Notification::warning("上一项结构变更尚未完成，请稍候").autohide(true));
            cx.notify();
            return false;
        };
        self.pending_notification = Some(
            Notification::info("正在执行表结构变更，请稍候…")
                .id::<TableDdlNotification>()
                .autohide(false),
        );
        self.clear_ddl_notification = false;
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let mut completion = completion;
            let started_at = Instant::now();
            let query = if conn.driver == DriverKind::Postgres {
                Query::new(sql.clone()).transactional()
            } else {
                Query::new(sql.clone())
            };
            let result = svc.execute(&conn, &query).await;
            if let Err(error) = &result {
                tracing::error!(
                    operation = "sql_ddl",
                    connection_id = %conn.id,
                    driver = ?conn.driver,
                    connection = %conn.name,
                    sql_bytes = sql.len(),
                    error = %error,
                    "tree DDL failed"
                );
            }
            let completion_ms = started_at.elapsed().as_millis() as u64;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.ddl_gate.finish(mutation_token);
                let current_connection =
                    this.connection.as_ref().map(|current| &current.id) == Some(&conn.id);
                this.clear_ddl_notification = true;
                if !current_connection || !current_mutation {
                    this.pending_notification = Some(match &result {
                        Ok(_) => Notification::success(format!(
                            "{success_msg}（发起时的连接「{}」；当前树状态已变化，未自动刷新）",
                            conn.name
                        ))
                        .autohide(true),
                        Err(error) => Notification::error(
                            error.write_hint(&format!("发起时的连接「{}」执行失败", conn.name)),
                        )
                        .autohide(true),
                    });
                    if let Some(completion) = completion.take() {
                        completion(false, this, cx);
                    }
                    cx.notify();
                    return;
                }
                let success = result.is_ok();
                match &result {
                    Ok(_) => {
                        let database_ms = result
                            .as_ref()
                            .map_or(completion_ms, |output| output.elapsed_ms);
                        this.pending_notification = Some(
                            Notification::success(success_message(&success_msg, database_ms))
                                .autohide(true),
                        );
                        match after {
                            AfterDdl::None => {}
                            AfterDdl::ReloadSchema {
                                schema,
                                invalidated_table,
                            } => {
                                this.schema_cache
                                    .write()
                                    .invalidate_table(&schema, &invalidated_table);
                                clear_invalidated_table_state(
                                    &mut this.selected,
                                    &mut this.table_columns,
                                    &schema,
                                    &invalidated_table,
                                );
                                this.invalidate_tree_rows();
                                if this.expanded.contains_key(&schema) {
                                    this.load_tables_for(schema, cx);
                                }
                            }
                            AfterDdl::FullRefresh { invalidated_schema } => {
                                this.schema_cache
                                    .write()
                                    .invalidate_schema(&invalidated_schema);
                                if this.active_schema.as_deref()
                                    == Some(invalidated_schema.as_str())
                                {
                                    // 刷新后由 load_schemas 选择默认 schema。
                                    this.active_schema = None;
                                }
                                this.refresh(cx);
                            }
                        }
                    }
                    Err(error) => {
                        this.pending_notification =
                            Some(Notification::error(error.write_hint("执行失败")).autohide(true));
                    }
                }
                if let Some(completion) = completion.take() {
                    completion(success, this, cx);
                }
                cx.notify();
            });
            svc.append_history(&conn, &query, &result, false).await;
        })
        .detach();
        true
    }
}
