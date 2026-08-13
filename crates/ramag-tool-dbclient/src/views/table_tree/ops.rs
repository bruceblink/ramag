//! 树节点破坏性操作：清空表 / 删除表（视图）/ 删除库。
//! 右键菜单 → open_confirm 二次确认 → 异步 DDL（走 execute_with_history 留痕）→ 刷新 + toast

use std::{rc::Rc, time::Instant};

use gpui::{AppContext as _, Context, Entity, ParentElement, Window, px};
use gpui_component::WindowExt as _;
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use ramag_domain::entities::{
    ConnectionConfig, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES, Query, Value,
};
use ramag_ui::{open_bounded_prompt, open_confirm};

use super::{TableTreePanel, TreeEvent};

/// DDL 完成后的树刷新方式
enum AfterDdl {
    /// 树结构无变化（清空表）
    None,
    /// 成功后清理旧表状态，并重拉单个 schema 的表列表。
    ReloadSchema {
        schema: String,
        invalidated_table: String,
    },
    /// 成功后清理失效的活动 schema，并重拉整棵树。
    FullRefresh { invalidated_schema: String },
}

pub(super) struct TableDdlNotification;

type DdlCompletion =
    Box<dyn FnOnce(bool, &mut TableTreePanel, &mut Context<TableTreePanel>) + 'static>;

/// 表 / 视图行右键菜单：表设计能力统一进入“修改表”，视图仍保留定义查看。
pub(super) fn table_context_menu(
    menu: PopupMenu,
    entity: Entity<TableTreePanel>,
    schema: String,
    table: String,
    is_view: bool,
) -> PopupMenu {
    let menu = if is_view {
        let (s, t, ent) = (schema.clone(), table.clone(), entity.clone());
        menu.item(ramag_ui::menu_item("视图定义").on_click(move |_, _, app| {
            let (s, t) = (s.clone(), t.clone());
            ent.update(app, |this, cx| this.handle_show_ddl(s, t, true, cx));
        }))
    } else {
        let (s, t, ent) = (schema.clone(), table.clone(), entity.clone());
        let menu = menu.item(ramag_ui::menu_item("导出").on_click(move |_, _, app| {
            let (s, t) = (s.clone(), t.clone());
            ent.update(app, |this, cx| this.export_table_to_file(s, t, cx));
        }));
        let (s, t, ent) = (schema.clone(), table.clone(), entity.clone());
        menu.item(ramag_ui::menu_item("修改表").on_click(move |_, _, app| {
            ent.update(app, |this, cx| {
                this.handle_modify_table(s.clone(), t.clone(), cx)
            });
        }))
    }
    .separator();

    let menu = if is_view {
        let (s, t, ent) = (schema.clone(), table.clone(), entity.clone());
        menu.item(ramag_ui::menu_item("改名").on_click(move |_, window, app| {
            let (s, t, ent) = (s.clone(), t.clone(), ent.clone());
            open_bounded_prompt(
                "重命名视图",
                format!("输入 {s}.{t} 的新名称"),
                &t.clone(),
                "改名",
                MAX_CONNECTION_IDENTIFIER_BYTES,
                move |new_name, _, app| {
                    ent.update(app, |this, cx| this.rename_table(s, t, new_name, true, cx));
                },
                window,
                app,
            );
        }))
    } else {
        menu
    };

    let menu = if is_view {
        menu
    } else {
        let (s, t, ent) = (schema.clone(), table.clone(), entity.clone());
        menu.item(
            ramag_ui::menu_item("清空表").on_click(move |_, window, app| {
                let (s, t, ent) = (s.clone(), t.clone(), ent.clone());
                open_confirm(
                    "清空表",
                    format!("将删除 {s}.{t} 的全部数据（TRUNCATE TABLE），此操作不可恢复。"),
                    "清空",
                    true,
                    move |_, app| {
                        ent.update(app, |this, cx| this.truncate_table(s, t, cx));
                    },
                    window,
                    app,
                );
            }),
        )
    };

    let (label, title, desc) = if is_view {
        (
            "删除视图",
            "删除视图",
            format!("将删除视图 {schema}.{table}（仅删除视图定义，不影响底层表数据）。"),
        )
    } else {
        (
            "删除表",
            "删除表",
            format!("将永久删除表 {schema}.{table}（表结构与数据一并删除），此操作不可恢复。"),
        )
    };
    menu.item(ramag_ui::menu_item(label).on_click(move |_, window, app| {
        let (s, t, ent) = (schema.clone(), table.clone(), entity.clone());
        open_confirm(
            title,
            desc.clone(),
            "删除",
            true,
            move |_, app| {
                ent.update(app, |this, cx| this.drop_table(s, t, is_view, cx));
            },
            window,
            app,
        );
    }))
}

/// schema 行右键菜单：导出 / 导入 + 删除库（MySQL：DROP DATABASE；PG：DROP SCHEMA … CASCADE）
pub(super) fn schema_context_menu(
    menu: PopupMenu,
    entity: Entity<TableTreePanel>,
    schema: String,
    driver: DriverKind,
) -> PopupMenu {
    let (s, ent) = (schema.clone(), entity.clone());
    let menu = menu.item(ramag_ui::menu_item("导出").on_click(move |_, _, app| {
        let (s, ent) = (s.clone(), ent.clone());
        ent.update(app, |this, cx| this.export_schema_to_file(s, cx));
    }));
    let (s, ent) = (schema.clone(), entity.clone());
    let menu = menu.item(
        ramag_ui::menu_item("导入库").on_click(move |_, window, app| {
            let (s, ent) = (s.clone(), ent.clone());
            ramag_ui::open_import_options_dialog(
                "导入库",
                format!(
                    "选择冲突策略与 .sql 文件（可多选）。ramag 导出的文件将导入到文件内\
                         记录的库；普通 .sql 以当前库 {s} 为默认目标。重复导入同一文件：\
                         「跳过」按对象断点续传，「合并」按行去重补齐，「覆盖」完全重建（幂等）。"
                ),
                true,
                ("SQL", &["sql"]),
                move |policy, files, _, app| {
                    ent.update(app, |this, cx| {
                        this.import_schema_from_files(s, policy, files, cx);
                    });
                },
                window,
                app,
            );
        }),
    );
    let (s, ent) = (schema.clone(), entity.clone());
    let menu = menu
        .item(
            ramag_ui::menu_item("导入表").on_click(move |_, window, app| {
                let (s, ent) = (s.clone(), ent.clone());
                ramag_ui::open_import_options_dialog(
                    "导入表",
                    format!(
                        "选择由 Ramag 表节点“导出”生成的 .sql 文件（可多选），恢复表结构、约束、索引和全部数据到库 {s}。为避免 SQL 跨库误写，文件所属库必须与当前库一致。"
                    ),
                    true,
                    ("SQL", &["sql"]),
                    move |policy, files, _, app| {
                        ent.update(app, |this, cx| {
                            this.import_structured_tables_from_files(s, policy, files, cx);
                        });
                    },
                    window,
                    app,
                );
            }),
        )
        .separator();
    // schema 重命名仅 PG 支持（ALTER SCHEMA … RENAME TO）；MySQL 官方已移除 RENAME DATABASE
    let menu = if matches!(driver, DriverKind::Postgres) {
        let (s, ent) = (schema.clone(), entity.clone());
        menu.item(ramag_ui::menu_item("改名").on_click(move |_, window, app| {
            let (s, ent) = (s.clone(), ent.clone());
            open_bounded_prompt(
                "改名",
                format!("输入 schema {s} 的新名称"),
                &s.clone(),
                "改名",
                MAX_CONNECTION_IDENTIFIER_BYTES,
                move |new_name, _, app| {
                    ent.update(app, |this, cx| this.rename_schema(s, new_name, cx));
                },
                window,
                app,
            );
        }))
    } else {
        menu
    };

    let (label, title, desc) = match driver {
        DriverKind::Postgres => (
            "删除",
            "删除",
            format!(
                "将永久删除 schema {schema} 及其中全部对象（DROP SCHEMA … CASCADE），此操作不可恢复。"
            ),
        ),
        _ => (
            "删除",
            "删除",
            format!("将永久删除数据库 {schema} 及其中全部表与数据，此操作不可恢复。"),
        ),
    };
    menu.item(ramag_ui::menu_item(label).on_click(move |_, window, app| {
        let (schema, ent) = (schema.clone(), entity.clone());
        open_confirm(
            title,
            desc.clone(),
            "删除",
            true,
            move |_, app| {
                ent.update(app, |this, cx| this.drop_schema(schema, cx));
            },
            window,
            app,
        );
    }))
}

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
                let _ = entity.update_in(async_cx, |_, window, cx| match result {
                    Ok(columns) => designer
                        .update(cx, |designer, cx| designer.set_columns(columns, window, cx)),
                    Err(error) => designer.update(cx, |designer, cx| {
                        designer.set_load_error(error.write_hint("加载表字段失败"), cx)
                    }),
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

    /// 仅 PG（菜单层已限制）
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

    /// 统一执行入口：成功按 after 刷新树，失败 toast 错误；均写查询历史
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
                        Err(error) => {
                            tracing::error!(
                                operation = "sql_ddl",
                                connection_id = %conn.id,
                                driver = ?conn.driver,
                                error = %error,
                                connection = %conn.name,
                                sql_bytes = sql.len(),
                                "tree DDL failed after connection change"
                            );
                            Notification::error(
                                error.write_hint(&format!("发起时的连接「{}」执行失败", conn.name)),
                            )
                            .autohide(true)
                        }
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
                                    // 让 refresh 后的 load_schemas 自动激活默认 schema 并广播。
                                    this.active_schema = None;
                                }
                                this.refresh(cx);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            operation = "sql_ddl",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            error = %e,
                            sql_bytes = sql.len(),
                            "tree DDL failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("执行失败")).autohide(true));
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

fn success_message(message: &str, elapsed_ms: u64) -> String {
    if elapsed_ms < 1_000 {
        return message.to_string();
    }
    format!(
        "{message}（数据库耗时 {:.1} 秒）",
        elapsed_ms as f64 / 1_000.0
    )
}

fn clear_invalidated_table_state(
    selected: &mut Option<(String, String)>,
    table_columns: &mut std::collections::HashMap<(String, String), super::TableColumns>,
    schema: &str,
    table: &str,
) {
    if selected
        .as_ref()
        .is_some_and(|(selected_schema, selected_table)| {
            selected_schema == schema && selected_table == table
        })
    {
        *selected = None;
    }
    table_columns.remove(&(schema.to_string(), table.to_string()));
}

async fn load_table_ddl(
    service: &ramag_app::ConnectionService,
    connection: &ConnectionConfig,
    schema: &str,
    table: &str,
) -> anyhow::Result<String> {
    let sql = ramag_domain::entities::build_ddl_query(connection.driver, schema, table, false);
    let result = service.execute(connection, &Query::new(sql)).await?;
    let ddl = result
        .rows
        .first()
        .and_then(|row| row.values.iter().rev().find_map(value_as_ddl))
        .ok_or_else(|| anyhow::anyhow!("数据库未返回建表语句"))?;
    Ok(ddl)
}

fn value_as_ddl(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::Json(value) => Some(value.to_string()),
        _ => None,
    }
}

fn ddl_truncate_table(driver: DriverKind, schema: &str, table: &str) -> String {
    format!(
        "TRUNCATE TABLE {}.{}",
        driver.quote_identifier(schema),
        driver.quote_identifier(table)
    )
}

fn ddl_drop_table(driver: DriverKind, schema: &str, table: &str, is_view: bool) -> String {
    let kind = if is_view { "VIEW" } else { "TABLE" };
    format!(
        "DROP {kind} {}.{}",
        driver.quote_identifier(schema),
        driver.quote_identifier(table)
    )
}

/// MySQL：RENAME TABLE（表 / 视图通用，新名带 schema）；PG：ALTER TABLE/VIEW … RENAME TO（新名不带 schema）
fn ddl_rename_table(
    driver: DriverKind,
    schema: &str,
    old: &str,
    new: &str,
    is_view: bool,
) -> String {
    let qs = driver.quote_identifier(schema);
    let qo = driver.quote_identifier(old);
    let qn = driver.quote_identifier(new);
    match driver {
        DriverKind::Postgres => {
            let kind = if is_view { "VIEW" } else { "TABLE" };
            format!("ALTER {kind} {qs}.{qo} RENAME TO {qn}")
        }
        _ => format!("RENAME TABLE {qs}.{qo} TO {qs}.{qn}"),
    }
}

/// MySQL 的 schema 即 database；PG 树展示的是 schema，加 CASCADE 才能删非空 schema
fn ddl_drop_schema(driver: DriverKind, schema: &str) -> String {
    let q = driver.quote_identifier(schema);
    match driver {
        DriverKind::Postgres => format!("DROP SCHEMA {q} CASCADE"),
        _ => format!("DROP DATABASE {q}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn truncate_quotes_by_dialect() {
        assert_eq!(
            ddl_truncate_table(DriverKind::Mysql, "shop", "order"),
            "TRUNCATE TABLE `shop`.`order`"
        );
        assert_eq!(
            ddl_truncate_table(DriverKind::Postgres, "public", "order"),
            "TRUNCATE TABLE \"public\".\"order\""
        );
    }

    #[test]
    fn drop_table_and_view() {
        assert_eq!(
            ddl_drop_table(DriverKind::Mysql, "shop", "t1", false),
            "DROP TABLE `shop`.`t1`"
        );
        assert_eq!(
            ddl_drop_table(DriverKind::Postgres, "public", "v1", true),
            "DROP VIEW \"public\".\"v1\""
        );
    }

    #[test]
    fn drop_schema_dialect_split() {
        assert_eq!(
            ddl_drop_schema(DriverKind::Mysql, "shop"),
            "DROP DATABASE `shop`"
        );
        assert_eq!(
            ddl_drop_schema(DriverKind::Postgres, "app"),
            "DROP SCHEMA \"app\" CASCADE"
        );
    }

    /// 标识符内引号必须转义，防注入式构造
    #[test]
    fn identifier_escaping() {
        assert_eq!(
            ddl_drop_schema(DriverKind::Mysql, "a`b"),
            "DROP DATABASE `a``b`"
        );
    }

    #[test]
    fn rename_table_dialect_split() {
        assert_eq!(
            ddl_rename_table(DriverKind::Mysql, "shop", "t1", "t2", false),
            "RENAME TABLE `shop`.`t1` TO `shop`.`t2`"
        );
        assert_eq!(
            ddl_rename_table(DriverKind::Postgres, "public", "t1", "t2", false),
            "ALTER TABLE \"public\".\"t1\" RENAME TO \"t2\""
        );
        assert_eq!(
            ddl_rename_table(DriverKind::Postgres, "public", "v1", "v2", true),
            "ALTER VIEW \"public\".\"v1\" RENAME TO \"v2\""
        );
    }

    #[test]
    fn successful_table_ddl_clears_only_invalidated_local_state() {
        let mut selected = Some(("public".to_string(), "users".to_string()));
        let mut columns = HashMap::from([
            (
                ("public".to_string(), "users".to_string()),
                super::super::TableColumns::default(),
            ),
            (
                ("public".to_string(), "posts".to_string()),
                super::super::TableColumns::default(),
            ),
        ]);

        clear_invalidated_table_state(&mut selected, &mut columns, "public", "users");

        assert!(selected.is_none());
        assert!(!columns.contains_key(&("public".into(), "users".into())));
        assert!(columns.contains_key(&("public".into(), "posts".into())));
    }

    #[test]
    fn slow_ddl_success_message_reports_database_time() {
        assert_eq!(success_message("已修改表", 999), "已修改表");
        assert_eq!(
            success_message("已修改表", 9_050),
            "已修改表（数据库耗时 9.1 秒）"
        );
    }
}
