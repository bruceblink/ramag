use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px,
};
use gpui_component::{
    Disableable as _, IconName, Sizable as _, Theme,
    button::ButtonVariants as _,
    h_flex,
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};

use super::super::super::schema_migration::MigrationScript;
use super::super::{DIFF_VIEW_HEIGHT, DIFF_VIEW_WIDTH, SchemaDiffDialog};
use super::approval;

fn render_migration_scrollable(
    dialog: &SchemaDiffDialog,
    content: impl IntoElement,
    theme: &Theme,
) -> AnyElement {
    // Keep the SQL preview fixed-width while exposing both scroll axes for long scripts.
    div()
        .relative()
        .h(px(DIFF_VIEW_HEIGHT))
        .w_full()
        .child(
            div()
                .id("schema-migration-horizontal-scroll")
                .size_full()
                .overflow_x_scroll()
                .track_scroll(&dialog.migration_horizontal_scroll)
                .child(
                    div()
                        .id("schema-migration-vertical-scroll")
                        .w(px(DIFF_VIEW_WIDTH))
                        .h_full()
                        .overflow_y_scroll()
                        .track_scroll(&dialog.migration_vertical_scroll)
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
                    Scrollbar::vertical(&dialog.migration_vertical_scroll)
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
                    Scrollbar::horizontal(&dialog.migration_horizontal_scroll)
                        .id("schema-migration-horizontal-scrollbar")
                        .scrollbar_show(ScrollbarShow::Always),
                ),
        )
        .into_any_element()
}

impl SchemaDiffDialog {
    /// Renders preview, approval history, and guarded actions for the current migration script.
    pub(in crate::views::schema_diff_dialog) fn render_migration_panel(
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
        if let Some(history) =
            approval::render_migration_approval_history(&self.migration_approvals, theme)
        {
            content = content.child(history);
        }
        content = content.child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .whitespace_nowrap()
                .child(script.sql.clone()),
        );
        render_migration_scrollable(self, content, theme)
    }
}
