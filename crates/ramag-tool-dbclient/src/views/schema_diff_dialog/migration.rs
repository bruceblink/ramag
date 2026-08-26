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

use super::super::schema_migration::{MigrationScript, build_migration_script};
use super::{DIFF_VIEW_HEIGHT, DIFF_VIEW_WIDTH, SchemaDiffDialog};

enum MigrationExportOutcome {
    Saved(PathBuf),
    Cancelled,
    Failed { path: PathBuf, error: String },
}

impl SchemaDiffDialog {
    pub(super) fn toggle_migration(&mut self, cx: &mut Context<Self>) {
        self.migration_visible = !self.migration_visible;
        self.migration_vertical_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        self.migration_horizontal_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        cx.notify();
    }

    pub(super) fn save_migration_sql(&mut self, cx: &mut Context<Self>) {
        if self.saving_migration {
            return;
        }
        let Some((source, target)) = self.source.as_ref().zip(self.target.as_ref()) else {
            return;
        };
        let script = match build_migration_script(
            self.target_connection.driver,
            &self.source_schema,
            &self.source_table,
            &self.target_schema,
            &self.target_table,
            &source.metadata,
            &target.metadata,
        ) {
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
                            .child(if has_statements {
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
                            .disabled(!has_statements || self.saving_migration)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.save_migration_sql(cx)
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("脚本只做预览和保存，不会自动执行；含删除或修改语句时请先人工复核。"),
            );
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
