use super::*;

use crate::views::table_designer::diff::{format_field_diff, render_field_diff_lines};

impl Render for TableDesigner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field_editor = self.render_field_editor(cx);
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted;
        let muted_fg = theme.muted_foreground;
        let entity = cx.entity().clone();
        if self.loading {
            return v_flex()
                .w_full()
                .h(px(360.0))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new().small())
                .child(div().text_sm().child("正在加载字段结构…"));
        }
        if let Some(error) = &self.load_error {
            return v_flex()
                .w_full()
                .h(px(300.0))
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("字段结构加载失败"),
                )
                .child(
                    div()
                        .max_w(px(680.0))
                        .text_xs()
                        .text_color(muted_fg)
                        .child(error.clone()),
                );
        }
        let active_fields = self.fields.iter().filter(|field| !field.deleted).count();
        let reviewing = self.preview_sql.is_some();
        let add = entity.clone();
        let preview = entity.clone();
        let execute = entity.clone();
        let rename = entity.clone();
        let toggle_ddl = entity.clone();
        let preview_cancel = entity.clone();
        let edit_cancel = entity.clone();
        let continue_editing = entity.clone();
        let executing = self.executing;
        let has_field_changes = self.has_field_changes(cx);
        let has_table_name_change = self.has_table_name_change(cx);
        let show_ddl = self.show_ddl;
        let discard_confirming = self.discard_confirming;
        let preview_sql = if discard_confirming {
            None
        } else {
            self.preview_sql.clone()
        };
        let preview_diff = if discard_confirming {
            None
        } else {
            self.preview_diff.clone()
        };
        v_flex()
            .debug_selector(|| "table-designer-content".into())
            .w_full()
            .gap_3()
            .child(
                ramag_ui::responsive_toolbar()
                    .debug_selector(|| "table-designer-top-toolbar".into())
                    .flex_none()
                    .items_end()
                    .justify_between()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_xs().text_color(muted_fg).child("表名"))
                            .child(
                                ramag_ui::responsive_toolbar()
                                    .min_w_0()
                                    .child(
                                        Input::new(&self.table_name)
                                            .w(px(320.0))
                                            .max_w(px(320.0))
                                            .flex_1()
                                            .min_w_0()
                                            .disabled(reviewing || executing || show_ddl),
                                    )
                                    .when(has_table_name_change, |name| {
                                        name.child(
                                            ramag_ui::clickable_button("table-designer-save-name")
                                                .debug_selector(|| {
                                                    "table-designer-save-name".into()
                                                })
                                                .primary()
                                                .small()
                                                .label(if executing {
                                                    "保存中…"
                                                } else {
                                                    "保存"
                                                })
                                                .loading(executing)
                                                .disabled(reviewing || executing || show_ddl)
                                                .on_click(move |_: &ClickEvent, window, app| {
                                                    rename.update(app, |this, cx| {
                                                        this.save_table_name(window, cx)
                                                    });
                                                }),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        ramag_ui::clickable_button("table-designer-show-ddl")
                            .debug_selector(|| "table-designer-show-ddl".into())
                            .secondary()
                            .small()
                            .flex_none()
                            .label(if show_ddl {
                                "字段设计"
                            } else {
                                "建表语句"
                            })
                            .disabled(reviewing || executing)
                            .on_click(move |_: &ClickEvent, _, app| {
                                toggle_ddl.update(app, |this, cx| {
                                    this.show_ddl = !this.show_ddl;
                                    this.discard_confirming = false;
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .when(show_ddl, |designer| {
                designer.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("建表语句"),
                        )
                        .child(render_ddl_panel(
                            self.ddl_loading,
                            self.ddl_text.clone(),
                            self.ddl_error.clone(),
                            &self.sql_scroll,
                            theme,
                        )),
                )
            })
            .when(!show_ddl, |designer| {
                designer.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("字段结构"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_fg)
                                        .child(format!("{active_fields} 个字段")),
                                ),
                        )
                        .child(field_editor),
                )
            })
            .when_some(preview_sql, |designer, sql| {
                let mono = theme.mono_font_family.clone();
                let sql_for_copy = sql.clone();
                let highlighted_sql = highlight_sql(sql, &theme.highlight_theme);
                let diff = preview_diff.clone().unwrap_or_default();
                let diff_for_copy = format_field_diff(&diff);
                let diff_header = ramag_ui::responsive_toolbar()
                    .flex_none()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("字段结构差异"),
                    )
                    .child(
                        ramag_ui::clickable_button("table-designer-copy-diff")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("复制字段差异")
                            .on_click(move |_: &ClickEvent, window, app| {
                                ramag_ui::copy_text_with_notification(
                                    diff_for_copy.clone(),
                                    window,
                                    app,
                                );
                            }),
                    );
                let diff_body = v_flex()
                    .w_full()
                    .gap_1()
                    .child(diff_header)
                    .child(render_field_diff_lines(&diff, theme));
                designer.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .p_3()
                        .border_1()
                        .border_color(border)
                        .rounded_lg()
                        .bg(muted.opacity(0.55))
                        .when(!discard_confirming, |preview| {
                            preview
                                .child(
                                    ramag_ui::responsive_toolbar()
                                        .flex_none()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("SQL 预览"),
                                        )
                                        .child(
                                            h_flex().items_center().gap_2().child(
                                                ramag_ui::clickable_button(
                                                    "table-designer-copy-sql",
                                                )
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Copy)
                                                .tooltip("复制 SQL")
                                                .on_click(move |_: &ClickEvent, window, app| {
                                                    ramag_ui::copy_text_with_notification(
                                                        sql_for_copy.clone(),
                                                        window,
                                                        app,
                                                    );
                                                }),
                                            ),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(SQL_PREVIEW_LINE_HEIGHT * SQL_PREVIEW_VISIBLE_LINES
                                            + SQL_PREVIEW_VERTICAL_PADDING))
                                        .id("table-designer-sql-preview-scroll")
                                        .overflow_y_scroll()
                                        .track_scroll(&self.sql_scroll)
                                        .p_3()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(border)
                                        .bg(theme.background)
                                        .text_xs()
                                        .line_height(px(SQL_PREVIEW_LINE_HEIGHT))
                                        .font_family(mono)
                                        .whitespace_normal()
                                        .child(
                                            v_flex()
                                                .w_full()
                                                .gap_3()
                                                .child(highlighted_sql)
                                                .child(diff_body),
                                        ),
                                )
                                .child(
                                    ramag_ui::responsive_toolbar()
                                        .flex_none()
                                        .justify_between()
                                        .child(
                                            ramag_ui::clickable_button("modify-table-back-to-edit")
                                                .ghost()
                                                .small()
                                                .label("返回")
                                                .disabled(executing)
                                                .on_click({
                                                    let entity = entity.clone();
                                                    move |_: &ClickEvent, _, app| {
                                                        entity.update(app, |this, cx| {
                                                            this.preview_sql = None;
                                                            cx.notify();
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    ramag_ui::clickable_button(
                                                        "modify-table-preview-cancel",
                                                    )
                                                    .ghost()
                                                    .small()
                                                    .label("取消")
                                                    .disabled(executing)
                                                    .on_click(move |_: &ClickEvent, window, app| {
                                                        preview_cancel.update(app, |this, cx| {
                                                            this.request_close(window, cx)
                                                        });
                                                    }),
                                                )
                                                .child(
                                                    ramag_ui::clickable_button(
                                                        "modify-table-confirm-execute",
                                                    )
                                                    .primary()
                                                    .small()
                                                    .label(if executing {
                                                        "执行中…"
                                                    } else {
                                                        "确认执行"
                                                    })
                                                    .loading(executing)
                                                    .disabled(executing)
                                                    .on_click(move |_: &ClickEvent, window, app| {
                                                        execute.update(app, |this, cx| {
                                                            this.confirm_execute(window, cx)
                                                        });
                                                    }),
                                                ),
                                        ),
                                )
                        }),
                )
            })
            .when(
                self.preview_sql.is_none() && !discard_confirming,
                |designer| {
                    designer.child(
                        ramag_ui::responsive_toolbar()
                            .flex_none()
                            .justify_between()
                            .when(!show_ddl, |actions| {
                                actions.child(
                                    ramag_ui::clickable_button("field-add")
                                        .secondary()
                                        .small()
                                        .label("添加字段")
                                        .disabled(executing)
                                        .on_click(move |_, window, app| {
                                            add.update(app, |this, cx| this.add_field(window, cx))
                                        }),
                                )
                            })
                            .when(show_ddl, |actions| actions.child(div()))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        ramag_ui::clickable_button("modify-table-cancel")
                                            .ghost()
                                            .small()
                                            .label("取消")
                                            .on_click(move |_: &ClickEvent, window, app| {
                                                edit_cancel.update(app, |this, cx| {
                                                    this.request_close(window, cx)
                                                });
                                            }),
                                    )
                                    .when(has_field_changes && !show_ddl, |actions| {
                                        actions.child(
                                            ramag_ui::clickable_button("modify-table-preview")
                                                .primary()
                                                .small()
                                                .label("预览")
                                                .disabled(executing || has_table_name_change)
                                                .when(has_table_name_change, |button| {
                                                    button.tooltip("请先保存表名")
                                                })
                                                .on_click(move |_: &ClickEvent, window, app| {
                                                    preview.update(app, |this, cx| {
                                                        match this.build_preview(cx) {
                                                            Ok(()) => {}
                                                            Err(error) => window.push_notification(
                                                                Notification::warning(error)
                                                                    .autohide(true),
                                                                cx,
                                                            ),
                                                        }
                                                    });
                                                }),
                                        )
                                    }),
                            ),
                    )
                },
            )
            .when(discard_confirming, |designer| {
                designer.child(
                    ramag_ui::responsive_toolbar()
                        .flex_none()
                        .items_start()
                        .justify_between()
                        .gap_3()
                        .p_3()
                        .border_1()
                        .border_color(theme.danger.opacity(0.35))
                        .rounded_lg()
                        .bg(theme.danger.opacity(0.08))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("放弃更改？"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_fg)
                                        .child("未保存的更改将丢失。"),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .child(
                                    ramag_ui::clickable_button("modify-table-continue-editing")
                                        .secondary()
                                        .small()
                                        .label("继续编辑")
                                        .on_click(move |_: &ClickEvent, _, app| {
                                            continue_editing.update(app, |this, cx| {
                                                this.discard_confirming = false;
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    ramag_ui::clickable_button("modify-table-discard-changes")
                                        .danger()
                                        .small()
                                        .label("放弃更改")
                                        .on_click(|_: &ClickEvent, window, app| {
                                            window.close_dialog(app)
                                        }),
                                ),
                        ),
                )
            })
    }
}
