use super::*;

impl Render for TableDesigner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted;
        let muted_fg = theme.muted_foreground;
        let syntax = &theme.highlight_theme.style.syntax;
        let type_color = syntax_color(syntax, "type", theme.link);
        let keyword_color = syntax_color(syntax, "keyword", theme.info);
        let number_color = syntax_color(syntax, "number", theme.warning);
        let string_color = syntax_color(syntax, "string", theme.success);
        let constant_color = syntax_color(syntax, "constant", theme.foreground);
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
        let mut rows = v_flex().w_full();
        let reviewing = self.preview_sql.is_some();
        let visible_rows = visible_field_rows(active_fields, reviewing);
        let rows_height = px(visible_rows as f32 * FIELD_ROW_HEIGHT);
        for (index, field) in self.fields.iter().enumerate() {
            if field.deleted {
                continue;
            }
            let toggle = entity.clone();
            let remove = entity.clone();
            rows =
                rows.child(
                    h_flex()
                        .w_full()
                        .min_h(px(46.0))
                        .items_center()
                        .gap_2()
                        .px_3()
                        .border_t_1()
                        .border_color(border)
                        .when(index % 2 == 1, |row| row.bg(muted.opacity(0.45)))
                        .child(
                            Input::new(&field.name)
                                .w(px(170.0))
                                .disabled(reviewing || self.executing),
                        )
                        .child(
                            Input::new(&field.data_type)
                                .w(px(180.0))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(type_color)
                                .disabled(reviewing || self.executing),
                        )
                        .child(
                            h_flex().w(px(76.0)).items_center().justify_center().child(
                                ramag_ui::clickable_checkbox(format!("field-nullable-{index}"))
                                    .checked(field.nullable)
                                    .small()
                                    .disabled(reviewing || self.executing)
                                    .tooltip("允许 NULL")
                                    .on_click(move |nullable: &bool, _, app| {
                                        toggle.update(app, |this, cx| {
                                            if let Some(field) = this.fields.get_mut(index) {
                                                field.nullable = *nullable;
                                            }
                                            this.preview_sql = None;
                                            this.discard_confirming = false;
                                            cx.notify();
                                        })
                                    }),
                            ),
                        )
                        .child(
                            Input::new(&field.default_value)
                                .w(px(180.0))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(default_value_color(
                                    field.default_value.read(cx).value().as_ref(),
                                    keyword_color,
                                    number_color,
                                    string_color,
                                    constant_color,
                                ))
                                .disabled(reviewing || self.executing),
                        )
                        .child(div().flex_1().min_w(px(150.0)).child(
                            Input::new(&field.comment).disabled(reviewing || self.executing),
                        ))
                        .child(
                            ramag_ui::clickable_button(format!("field-delete-{index}"))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Delete)
                                .tooltip("删除")
                                .text_color(theme.danger)
                                .disabled(reviewing || self.executing)
                                .on_click(move |_: &ClickEvent, _, app| {
                                    remove.update(app, |this, cx| {
                                        if let Some(field) = this.fields.get_mut(index) {
                                            field.deleted = true;
                                        }
                                        this.preview_sql = None;
                                        this.discard_confirming = false;
                                        cx.notify();
                                    })
                                }),
                        ),
                );
        }
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
        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted_fg).child("表名"))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Input::new(&self.table_name)
                                            .w(px(320.0))
                                            .disabled(reviewing || executing || show_ddl),
                                    )
                                    .when(has_table_name_change, |name| {
                                        name.child(
                                            ramag_ui::clickable_button("table-designer-save-name")
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
                            .secondary()
                            .small()
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
                        .child(
                            v_flex()
                                .w_full()
                                .border_1()
                                .border_color(border)
                                .rounded_lg()
                                .overflow_hidden()
                                .child(
                                    h_flex()
                                        .min_h(px(38.0))
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .bg(muted.opacity(0.7))
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(muted_fg)
                                        .child(div().w(px(170.0)).child("字段名"))
                                        .child(div().w(px(180.0)).child("类型"))
                                        .child(div().w(px(76.0)).text_center().child("允许 NULL"))
                                        .child(div().w(px(180.0)).child("默认值"))
                                        .child(div().flex_1().child("注释"))
                                        .child(div().w(px(36.0)).text_center().child("操作")),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .h(rows_height)
                                        .min_h_0()
                                        .flex_none()
                                        .id("table-designer-fields-scroll")
                                        .overflow_y_scroll()
                                        .track_scroll(&self.field_scroll)
                                        .child(rows),
                                ),
                        ),
                )
            })
            .when_some(preview_sql, |designer, sql| {
                let mono = theme.mono_font_family.clone();
                let highlighted_sql = highlight_sql(sql, &theme.highlight_theme);
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
                                    h_flex()
                                        .w_full()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("SQL 预览"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted_fg)
                                                .child("确认后执行"),
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
                                        .child(highlighted_sql),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .items_center()
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
                        h_flex()
                            .w_full()
                            .items_center()
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
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .p_3()
                        .border_1()
                        .border_color(theme.danger.opacity(0.35))
                        .rounded_lg()
                        .bg(theme.danger.opacity(0.08))
                        .child(
                            v_flex()
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
