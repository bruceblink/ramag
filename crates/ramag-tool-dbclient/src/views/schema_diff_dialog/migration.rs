use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px,
};
use gpui_component::{
    Disableable as _, IconName, Sizable as _, Theme,
    button::ButtonVariants as _,
    h_flex,
    notification::Notification,
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};
use ramag_app::usecases::export;
use ramag_domain::entities::{DriverKind, Query};

use super::super::schema_migration::{MigrationScript, build_migration_script};
use super::{DIFF_VIEW_HEIGHT, DIFF_VIEW_WIDTH, SchemaDiffDialog};

enum MigrationExportOutcome {
    Saved(PathBuf),
    Cancelled,
    Failed { path: PathBuf, error: String },
}

impl SchemaDiffDialog {
    /// Rebuilds the script from the currently loaded metadata so save and execute never use a stale preview.
    fn current_migration_script(&self) -> Result<MigrationScript, String> {
        let Some((source, target)) = self.source.as_ref().zip(self.target.as_ref()) else {
            return Err("两张表的元数据尚未加载，无法生成迁移 SQL".into());
        };
        if !source.warnings.is_empty() || !target.warnings.is_empty() {
            return Err("源表或目标表的元数据未完整加载，无法执行迁移 SQL；请刷新后重试".into());
        }
        if self.source_connection.driver != self.target_connection.driver {
            return Err("源连接和目标连接必须使用相同数据库驱动，无法执行迁移 SQL".into());
        }
        build_migration_script(
            self.target_connection.driver,
            &self.source_schema,
            &self.source_table,
            &self.target_schema,
            &self.target_table,
            &source.metadata,
            &target.metadata,
        )
    }

    pub(super) fn toggle_migration(&mut self, cx: &mut Context<Self>) {
        self.migration_visible = !self.migration_visible;
        self.migration_vertical_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        self.migration_horizontal_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        cx.notify();
    }

    pub(super) fn save_migration_sql(&mut self, cx: &mut Context<Self>) {
        if self.saving_migration || self.executing_migration {
            return;
        }
        let script = match self.current_migration_script() {
            Ok(script) => script,
            Err(error) => {
                self.pending_notification = Some(Notification::error(error).autohide(true));
                cx.notify();
                return;
            }
        };
        if script.statement_count == 0 {
            self.pending_notification =
                Some(Notification::info("结构已一致，无需保存迁移 SQL").autohide(true));
            cx.notify();
            return;
        }

        let database_type = match self.target_connection.driver {
            ramag_domain::entities::DriverKind::Mysql => "mysql",
            ramag_domain::entities::DriverKind::Postgres => "postgresql",
            _ => "sql",
        };
        let object = format!("{}_to_{}", self.source_table, self.target_table);
        let file_name = export::suggested_export_file_name(
            database_type,
            &self.target_schema,
            Some(&object),
            false,
            "sql",
        );
        let source_name = self.source_table.clone();
        let target_name = self.target_table.clone();
        let connection_id = self.target_connection.id.to_string();
        self.saving_migration = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = match rfd::AsyncFileDialog::new()
                .set_file_name(&file_name)
                .add_filter("SQL", &["sql"])
                .save_file()
                .await
            {
                None => MigrationExportOutcome::Cancelled,
                Some(handle) => {
                    let path = handle.path().to_path_buf();
                    let write_path = path.clone();
                    match ramag_app::run_blocking(move || {
                        export::write_atomic(&write_path, &script.sql)
                    })
                    .await
                    {
                        Ok(()) => MigrationExportOutcome::Saved(path),
                        Err(error) => MigrationExportOutcome::Failed {
                            path,
                            error: error.to_string(),
                        },
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.saving_migration = false;
                this.pending_save_notification(
                    outcome,
                    &connection_id,
                    &source_name,
                    &target_name,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Requests explicit approval, then executes the unchanged script against the target connection.
    pub(super) fn request_execute_migration(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving_migration || self.executing_migration {
            return;
        }
        if self.target_connection.production {
            self.pending_notification = Some(
                Notification::warning("目标连接已标记为生产环境，只能预览或保存迁移 SQL")
                    .autohide(true),
            );
            cx.notify();
            return;
        }
        let script = match self.current_migration_script() {
            Ok(script) => script,
            Err(error) => {
                self.pending_notification = Some(Notification::error(error).autohide(true));
                cx.notify();
                return;
            }
        };
        if script.statement_count == 0 {
            self.pending_notification =
                Some(Notification::info("结构已一致，无需执行迁移 SQL").autohide(true));
            cx.notify();
            return;
        }

        let transaction_note = match self.target_connection.driver {
            DriverKind::Postgres => "PostgreSQL 将在一个事务中执行，任一语句失败会回滚本次迁移。",
            DriverKind::Mysql => "MySQL DDL 可能隐式提交；执行失败时目标表可能已经部分变更。",
            _ => "当前数据库驱动不支持迁移 SQL 执行。",
        };
        let mut description = format!(
            "目标连接：{}\n目标表：{}.{}\n将执行 {} 条迁移语句，其中 {} 条删除或修改。\n{}\n此操作会直接修改目标数据库。",
            self.target_connection.name,
            self.target_schema,
            self.target_table,
            script.statement_count,
            script.destructive_statements,
            transaction_note,
        );
        if !script.warnings.is_empty() {
            description.push_str("\n生成提示：");
            for warning in script.warnings.iter().take(4) {
                description.push_str("\n- ");
                description.push_str(warning);
            }
        }

        let entity = cx.entity();
        let request_generation = self.request_generation;
        let target_connection_id = self.target_connection.id.clone();
        let source_schema = self.source_schema.clone();
        let source_table = self.source_table.clone();
        let target_schema = self.target_schema.clone();
        let target_table = self.target_table.clone();
        let expected_sql = script.sql.clone();
        ramag_ui::open_confirm(
            "执行迁移 SQL？",
            description,
            "确认执行",
            script.destructive_statements > 0,
            move |_, app| {
                entity.update(app, |this, cx| {
                    let context_changed = this.request_generation != request_generation
                        || this.target_connection.id != target_connection_id
                        || this.source_schema != source_schema
                        || this.source_table != source_table
                        || this.target_schema != target_schema
                        || this.target_table != target_table
                        || this
                            .current_migration_script()
                            .map_or(true, |current| current.sql != expected_sql);
                    if context_changed {
                        this.pending_notification = Some(
                            Notification::warning("迁移预览已变化，请重新打开预览并确认")
                                .autohide(true),
                        );
                        cx.notify();
                        return;
                    }
                    this.start_migration_execution(script, cx);
                });
            },
            window,
            cx,
        );
    }

    /// Runs the approved script once and reloads both sides so the dialog reflects the database.
    fn start_migration_execution(&mut self, script: MigrationScript, cx: &mut Context<Self>) {
        let execution_generation = self.migration_execution_generation.wrapping_add(1);
        self.migration_execution_generation = execution_generation;
        self.executing_migration = true;
        let service = self.service.clone();
        let target_connection = self.target_connection.clone();
        let target_schema = self.target_schema.clone();
        let target_table = self.target_table.clone();
        let query = migration_query(target_connection.driver, &target_schema, script.sql.clone());
        let statement_count = script.statement_count;
        let destructive_statements = script.destructive_statements;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = service.execute(&target_connection, &query).await;
            let _ = this.update(cx, |this, cx| {
                if this.migration_execution_generation != execution_generation {
                    return;
                }
                this.executing_migration = false;
                match result {
                    Ok(result) => {
                        let warning_count = result.warnings.len();
                        let warning_suffix = if warning_count == 0 {
                            String::new()
                        } else {
                            format!("，数据库返回 {warning_count} 条警告")
                        };
                        tracing::info!(
                            operation = "schema_migration_execute",
                            connection_id = %target_connection.id,
                            target_schema = %target_schema,
                            target_table = %target_table,
                            statements = statement_count,
                            destructive_statements,
                            elapsed_ms = result.elapsed_ms,
                            warning_count,
                            "schema migration executed"
                        );
                        this.pending_notification = Some(
                            Notification::success(format!(
                                "迁移执行成功：{statement_count} 条语句，耗时 {} ms{warning_suffix}；正在重新读取元数据",
                                result.elapsed_ms
                            ))
                            .autohide(true),
                        );
                        this.refresh(cx);
                    }
                    Err(error) => {
                        tracing::error!(
                            operation = "schema_migration_execute",
                            connection_id = %target_connection.id,
                            target_schema = %target_schema,
                            target_table = %target_table,
                            statements = statement_count,
                            destructive_statements,
                            error = %error,
                            "schema migration execution failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(format!("迁移执行失败：{error}")).autohide(true));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn pending_save_notification(
        &mut self,
        outcome: MigrationExportOutcome,
        connection_id: &str,
        source_table: &str,
        target_table: &str,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            MigrationExportOutcome::Cancelled => return,
            MigrationExportOutcome::Saved(path) => {
                tracing::info!(
                    operation = "schema_migration_export",
                    connection_id,
                    source_table,
                    target_table,
                    path = %path.display(),
                    "schema migration script exported"
                );
                let file_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.pending_notification = Some(
                    Notification::success(format!("已保存迁移 SQL：{file_name}")).autohide(true),
                );
            }
            MigrationExportOutcome::Failed { path, error } => {
                tracing::error!(
                    operation = "schema_migration_export",
                    connection_id,
                    source_table,
                    target_table,
                    path = %path.display(),
                    error = %error,
                    "schema migration script export failed"
                );
                self.pending_notification =
                    Some(Notification::error(format!("保存迁移 SQL 失败：{error}")).autohide(true));
            }
        }
        cx.notify();
    }

    fn render_migration_scrollable(&self, content: impl IntoElement, theme: &Theme) -> AnyElement {
        div()
            .relative()
            .h(px(DIFF_VIEW_HEIGHT))
            .w_full()
            .child(
                div()
                    .id("schema-migration-horizontal-scroll")
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.migration_horizontal_scroll)
                    .child(
                        div()
                            .id("schema-migration-vertical-scroll")
                            .w(px(DIFF_VIEW_WIDTH))
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.migration_vertical_scroll)
                            .child(content),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom(px(16.0))
                    .right_0()
                    .w(px(16.0))
                    .bg(theme.scrollbar)
                    .child(
                        Scrollbar::vertical(&self.migration_vertical_scroll)
                            .id("schema-migration-vertical-scrollbar")
                            .scrollbar_show(ScrollbarShow::Always),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(16.0))
                    .bg(theme.scrollbar)
                    .child(
                        Scrollbar::horizontal(&self.migration_horizontal_scroll)
                            .id("schema-migration-horizontal-scrollbar")
                            .scrollbar_show(ScrollbarShow::Always),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_migration_panel(
        &self,
        migration: Option<&Result<MigrationScript, String>>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(migration) = migration else {
            return v_flex()
                .h(px(DIFF_VIEW_HEIGHT))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("暂无可生成的迁移 SQL")
                .into_any_element();
        };
        let script = match migration {
            Ok(script) => script,
            Err(error) => {
                return v_flex()
                    .h(px(DIFF_VIEW_HEIGHT))
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .text_xs()
                    .text_color(theme.danger)
                    .child(error.clone())
                    .into_any_element();
            }
        };

        let has_statements = script.statement_count > 0;
        let copy_text = script.sql.clone();
        let mut content = v_flex()
            .w(px(DIFF_VIEW_WIDTH))
            .gap(px(8.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(14.0))
                    .text_xs()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if has_statements {
                                theme.warning
                            } else {
                                theme.success
                            })
                            .child(if self.executing_migration {
                                "正在执行迁移"
                            } else if has_statements {
                                "可生成迁移"
                            } else {
                                "结构已一致"
                            }),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} 条语句", script.statement_count)),
                    )
                    .when(script.destructive_statements > 0, |row| {
                        row.child(div().text_color(theme.danger).child(format!(
                            "含 {} 条删除或修改语句",
                            script.destructive_statements
                        )))
                    })
                    .child(div().flex_1())
                    .child(
                        ramag_ui::clickable_button("schema-migration-copy")
                            .ghost()
                            .small()
                            .icon(IconName::Copy)
                            .tooltip("复制迁移 SQL")
                            .on_click(move |_: &ClickEvent, window, app| {
                                ramag_ui::copy_text_with_notification(
                                    copy_text.clone(),
                                    window,
                                    app,
                                );
                            }),
                    )
                    .child(
                        ramag_ui::clickable_button("schema-migration-save")
                            .ghost()
                            .small()
                            .icon(IconName::File)
                            .tooltip("保存迁移 SQL")
                            .disabled(
                                !has_statements
                                    || self.saving_migration
                                    || self.executing_migration,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.save_migration_sql(cx)
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("schema-migration-execute")
                            .ghost()
                            .small()
                            .icon(IconName::Play)
                            .tooltip(if self.target_connection.production {
                                "生产连接禁止执行迁移"
                            } else if self.executing_migration {
                                "正在执行迁移"
                            } else {
                                "确认后执行迁移 SQL"
                            })
                            .disabled(
                                !has_statements
                                    || self.saving_migration
                                    || self.executing_migration
                                    || self.target_connection.production,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.request_execute_migration(window, cx)
                            })),
                    ),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                if self.target_connection.production {
                    "目标连接为生产环境，只能预览或保存迁移 SQL。"
                } else if self.executing_migration {
                    "正在执行迁移，完成后会自动重新读取两张表的元数据。"
                } else {
                    "执行会直接修改目标表；确认前请人工复核脚本，保存的脚本不会自动执行。"
                },
            ));
        if !script.warnings.is_empty() {
            content =
                content.child(
                    v_flex()
                        .w_full()
                        .gap(px(2.0))
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(theme.warning.opacity(0.08))
                        .children(script.warnings.iter().cloned().map(|warning| {
                            div().text_xs().text_color(theme.warning).child(warning)
                        })),
                );
        }
        content = content.child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .whitespace_nowrap()
                .child(script.sql.clone()),
        );
        self.render_migration_scrollable(content, theme)
    }
}

/// Builds the execution query with the transaction rule supported by each SQL dialect.
fn migration_query(driver: DriverKind, schema: &str, sql: String) -> Query {
    let query = Query::new(sql).with_schema(schema);
    if driver == DriverKind::Postgres {
        query.transactional()
    } else {
        query
    }
}

#[cfg(test)]
mod tests {
    use super::migration_query;
    use ramag_domain::entities::DriverKind;

    #[test]
    fn postgres_migration_query_is_transactional() {
        let query = migration_query(DriverKind::Postgres, "public", "SELECT 1".into());
        assert!(query.transactional);
        assert_eq!(query.default_schema.as_deref(), Some("public"));
    }

    #[test]
    fn mysql_migration_query_keeps_ddl_autocommit_behavior() {
        let query = migration_query(DriverKind::Mysql, "app", "SELECT 1".into());
        assert!(!query.transactional);
        assert_eq!(query.default_schema.as_deref(), Some("app"));
    }
}
