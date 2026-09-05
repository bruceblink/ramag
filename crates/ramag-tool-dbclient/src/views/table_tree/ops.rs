//! 表树结构变更操作。

use std::rc::Rc;

use gpui::{AppContext as _, Context, Entity, ParentElement, Window, px};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use ramag_domain::entities::DriverKind;

pub(super) use super::ddl_ops::TableDdlNotification;
pub(super) use super::menus::{schema_context_menu, table_context_menu};
use super::{
    TableTreePanel, TreeEvent,
    ddl::{ddl_drop_schema, ddl_drop_table, ddl_rename_table, ddl_truncate_table, load_table_ddl},
    ddl_ops::AfterDdl,
};

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
        if !matches!(
            driver,
            DriverKind::Mysql | DriverKind::Postgres | DriverKind::Sqlite
        ) {
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
        if driver == DriverKind::Sqlite && is_view {
            self.pending_notification = Some(
                Notification::warning("SQLite 不支持直接重命名视图，请在查询编辑器中重建视图")
                    .autohide(true),
            );
            cx.notify();
            return;
        }
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
        let sql = match ddl_drop_schema(driver, &schema) {
            Ok(sql) => sql,
            Err(error) => {
                self.pending_notification = Some(Notification::warning(error).autohide(true));
                cx.notify();
                return;
            }
        };
        self.exec_ddl(
            sql,
            format!("已删除 {schema}"),
            AfterDdl::FullRefresh {
                invalidated_schema: schema,
            },
            cx,
        );
    }

    pub(super) fn execute_metadata_ddl(
        &mut self,
        sql: String,
        success_msg: String,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((production, driver)) = self
            .connection
            .as_ref()
            .map(|config| (config.production, config.driver))
        else {
            return false;
        };
        if production {
            self.pending_notification = Some(
                Notification::warning("生产连接已启用只读保护，不能修改索引或触发器")
                    .autohide(true),
            );
            cx.notify();
            return false;
        }
        if !matches!(
            driver,
            DriverKind::Mysql | DriverKind::Postgres | DriverKind::Sqlite
        ) {
            self.pending_notification =
                Some(Notification::warning("当前数据库类型不支持索引或触发器操作").autohide(true));
            cx.notify();
            return false;
        }
        self.exec_ddl(
            sql,
            success_msg,
            AfterDdl::ReloadSchema {
                schema,
                invalidated_table: table,
            },
            cx,
        )
    }
}
