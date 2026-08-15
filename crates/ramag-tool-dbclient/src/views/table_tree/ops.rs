//! 表树结构变更操作。

use std::{rc::Rc, time::Instant};

use gpui::{AppContext as _, Context, Entity, ParentElement, Window, px};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use ramag_domain::entities::{DriverKind, Query};

pub(super) use super::menus::{schema_context_menu, table_context_menu};
use super::{
    TableTreePanel, TreeEvent,
    ddl::{
        clear_invalidated_table_state, ddl_drop_schema, ddl_drop_table, ddl_rename_table,
        ddl_truncate_table, load_table_ddl, success_message,
    },
};

enum AfterDdl {
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
    pub(crate) fn open_modify_table_dialog(
        &mut self,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(driver) = self.connection.as_ref().map(|config| config.driver) else {
            return;
        };
        if !matches!(driver, DriverKind::Mysql | DriverKind::Postgres) {
            self.pending_notification =
                Some(Notification::warning("当前数据库暂不支持表结构设计器").autohide(true));
            cx.notify();
            return;
        }
        let Some(connection) = self.connection.clone() else {
            return;
        };
        let ddl_loading = true;
        let columns = self
            .table_columns
            .get(&(schema.clone(), table.clone()))
            .filter(|value| !value.loading && value.error.is_none())
            .map(|value| value.columns.clone());
        let Some(columns) = columns else {
            let tree = cx.entity().clone();
            let designer = self.open_modify_table_with_columns(
                crate::views::table_designer::TableDesignerConfig {
                    driver,
                    schema: schema.clone(),
                    table: table.clone(),
                    columns: Vec::new(),
                    loading: true,
                    ddl_loading,
                    on_execute: Self::modify_table_execute_handler(&schema, tree.clone()),
                    on_rename: Self::rename_table_execute_handler(&schema, tree),
                },
                window,
                cx,
            );
            let service = self.service.clone();
            let entity = cx.entity().clone();
            cx.spawn_in(window, async move |_, async_cx| {
                let result = service.list_columns(&connection, &schema, &table).await;
                if let Err(error) = &result {
                    tracing::error!(
                        operation = "load_table_columns",
                        connection_id = %connection.id,
                        connection = %connection.name,
                        driver = ?connection.driver,
                        schema = %schema,
                        table = %table,
                        error = %error,
                        "table designer column loading failed"
                    );
                }
                let _ = entity.update_in(async_cx, |_, window, cx| match result {
                    Ok(columns) => designer
                        .update(cx, |designer, cx| designer.set_columns(columns, window, cx)),
                    Err(error) => {
                        designer.update(cx, |designer, cx| {
                            designer.set_load_error(error.write_hint("加载表字段失败"), cx)
                        });
                    }
                });
            })
            .detach();
            return;
        };
        let tree = cx.entity().clone();
        let on_execute = Self::modify_table_execute_handler(&schema, tree.clone());
        let on_rename = Self::rename_table_execute_handler(&schema, tree);
        self.open_modify_table_with_columns(
            crate::views::table_designer::TableDesignerConfig {
                driver,
                schema,
                table,
                columns,
                loading: false,
                ddl_loading,
                on_execute,
                on_rename,
            },
            window,
            cx,
        );
    }

    fn modify_table_execute_handler(
        schema: &str,
        tree: Entity<Self>,
    ) -> crate::views::table_designer::ExecuteHandler {
        let schema = schema.to_string();
        Rc::new(move |sql, table, _, app| {
            tree.update(app, |tree, cx| {
                tree.execute_modify_table(sql, schema.clone(), table, cx)
            })
        })
    }

    fn rename_table_execute_handler(
        schema: &str,
        tree: Entity<Self>,
    ) -> crate::views::table_designer::RenameHandler {
        let schema = schema.to_string();
        Rc::new(move |sql, old_table, new_table, designer, _, app| {
            tree.update(app, |tree, cx| {
                tree.execute_designer_rename(
                    sql,
                    schema.clone(),
                    old_table,
                    new_table,
                    designer,
                    cx,
                )
            })
        })
    }

    fn open_modify_table_with_columns(
        &mut self,
        config: crate::views::table_designer::TableDesignerConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::views::table_designer::TableDesigner> {
        let schema = config.schema.clone();
        let table = config.table.clone();
        let connection = self.connection.clone();
        let service = self.service.clone();
        let title = format!("修改表 · {schema}.{table}");
        let designer =
            cx.new(|cx| crate::views::table_designer::TableDesigner::new(config, window, cx));
        let designer_for_dialog = designer.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let designer_for_content = designer_for_dialog.clone();
            let designer_for_cancel = designer_for_dialog.clone();
            dialog
                .title(title.clone())
                .close_button(false)
                .on_cancel(move |_, _, app| {
                    designer_for_cancel.update(app, |designer, cx| designer.allow_dialog_close(cx))
                })
                .width(px(1080.0))
                .margin_top(px(70.0))
                .content(move |content, _, _| content.child(designer_for_content.clone()))
        });
        if let Some(connection) = connection {
            let designer_for_ddl = designer.clone();
            cx.spawn_in(window, async move |_, async_cx| {
                let result = load_table_ddl(&service, &connection, &schema, &table).await;
                if let Err(error) = &result {
                    tracing::error!(
                        operation = "load_table_ddl",
                        connection_id = %connection.id,
                        connection = %connection.name,
                        driver = ?connection.driver,
                        schema = %schema,
                        table = %table,
                        error = %error,
                        "table designer DDL loading failed"
                    );
                }
                let _ = designer_for_ddl.update_in(async_cx, |designer, _, cx| match result {
                    Ok(ddl) => designer.set_ddl(ddl, cx),
                    Err(error) => {
                        designer.set_ddl_error(format!("加载建表语句失败：{error:#}"), cx)
                    }
                });
            })
            .detach();
        }
        designer
    }

    fn execute_modify_table(
        &mut self,
        sql: String,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(config) = self.connection.as_ref() else {
            return false;
        };
        if config.production {
            self.pending_notification = Some(
                Notification::warning("生产连接已启用只读保护，不能修改表结构").autohide(true),
            );
            cx.notify();
            return false;
        }
        self.exec_ddl(
            sql,
            format!("已修改表 {schema}.{table}"),
            AfterDdl::ReloadSchema {
                schema,
                invalidated_table: table,
            },
            cx,
        )
    }

    fn execute_designer_rename(
        &mut self,
        sql: String,
        schema: String,
        old_table: String,
        new_table: String,
        designer: Entity<crate::views::table_designer::TableDesigner>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(config) = self.connection.as_ref() else {
            return false;
        };
        if config.production {
            self.pending_notification = Some(
                Notification::warning("生产连接已启用只读保护，不能修改表结构").autohide(true),
            );
            cx.notify();
            return false;
        }
        let schema_for_reload = schema.clone();
        let new_table_for_reload = new_table.clone();
        self.exec_ddl_with_completion(
            sql,
            format!("已重命名表 {schema}.{new_table}"),
            AfterDdl::ReloadSchema {
                schema,
                invalidated_table: old_table,
            },
            Some(Box::new(move |success, tree, cx| {
                designer.update(cx, |designer, cx| {
                    designer.finish_rename(success, new_table.clone(), cx)
                });
                if !success {
                    return;
                }
                let Some(connection) = tree.connection.clone() else {
                    return;
                };
                let service = tree.service.clone();
                cx.spawn(async move |_, cx| {
                    let result = load_table_ddl(
                        &service,
                        &connection,
                        &schema_for_reload,
                        &new_table_for_reload,
                    )
                    .await;
                    if let Err(error) = &result {
                        tracing::error!(
                            operation = "load_table_ddl_after_rename",
                            connection_id = %connection.id,
                            connection = %connection.name,
                            driver = ?connection.driver,
                            schema = %schema_for_reload,
                            table = %new_table_for_reload,
                            error = %error,
                            "renamed table DDL loading failed"
                        );
                    }
                    designer.update(cx, |designer, cx| match result {
                        Ok(ddl) => designer.set_ddl(ddl, cx),
                        Err(error) => {
                            designer.set_ddl_error(format!("加载建表语句失败：{error:#}"), cx)
                        }
                    });
                })
                .detach();
            })),
            cx,
        )
    }

    pub(super) fn handle_modify_table(
        &mut self,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TreeEvent::ModifyTable { schema, table });
    }

    pub(super) fn truncate_table(&mut self, schema: String, table: String, cx: &mut Context<Self>) {
        let Some(driver) = self.connection.as_ref().map(|c| c.driver) else {
            return;
        };
        let sql = ddl_truncate_table(driver, &schema, &table);
        self.exec_ddl(
            sql,
            format!("已清空表 {schema}.{table}"),
            AfterDdl::None,
            cx,
        );
    }

    pub(super) fn drop_table(
        &mut self,
        schema: String,
        table: String,
        is_view: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(driver) = self.connection.as_ref().map(|c| c.driver) else {
            return;
        };
        let sql = ddl_drop_table(driver, &schema, &table, is_view);
        let label = if is_view { "视图" } else { "表" };
        self.exec_ddl(
            sql,
            format!("已删除{label} {schema}.{table}"),
            AfterDdl::ReloadSchema {
                schema,
                invalidated_table: table,
            },
            cx,
        );
    }

    pub(super) fn rename_table(
        &mut self,
        schema: String,
        old: String,
        new: String,
        is_view: bool,
        cx: &mut Context<Self>,
    ) {
        if new == old {
            return;
        }
        let Some(driver) = self.connection.as_ref().map(|c| c.driver) else {
            return;
        };
        let sql = ddl_rename_table(driver, &schema, &old, &new, is_view);
        self.exec_ddl(
            sql,
            format!("已重命名为 {schema}.{new}"),
            AfterDdl::ReloadSchema {
                schema,
                invalidated_table: old,
            },
            cx,
        );
    }

    pub(super) fn rename_schema(&mut self, old: String, new: String, cx: &mut Context<Self>) {
        if new == old {
            return;
        }
        let Some(driver) = self.connection.as_ref().map(|c| c.driver) else {
            return;
        };
        let sql = format!(
            "ALTER SCHEMA {} RENAME TO {}",
            driver.quote_identifier(&old),
            driver.quote_identifier(&new)
        );
        self.exec_ddl(
            sql,
            format!("已重命名为 {new}"),
            AfterDdl::FullRefresh {
                invalidated_schema: old,
            },
            cx,
        );
    }

    pub(super) fn drop_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        let Some(driver) = self.connection.as_ref().map(|c| c.driver) else {
            return;
        };
        let sql = ddl_drop_schema(driver, &schema);
        self.exec_ddl(
            sql,
            format!("已删除 {schema}"),
            AfterDdl::FullRefresh {
                invalidated_schema: schema,
            },
            cx,
        );
    }

    fn exec_ddl(
        &mut self,
        sql: String,
        success_msg: String,
        after: AfterDdl,
        cx: &mut Context<Self>,
    ) -> bool {
        self.exec_ddl_with_completion(sql, success_msg, after, None, cx)
    }

    fn exec_ddl_with_completion(
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
