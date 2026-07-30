//! `impl Render for QueryTab`：编辑器 + 工具条 + 结果区。按钮行为在 actions

use gpui::{
    AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    notification::Notification,
    v_flex,
};
use ramag_domain::entities::MAX_SQL_QUERY_BYTES;
use ramag_ui::PointerDropdownMenu as _;

use super::QueryTab;
use super::sql_utils::format_elapsed;

/// 千分位格式化（10_000 → "10,000"），自动限制档位展示用
use crate::actions::{ExplainQuery, FormatSql, RunQuery, RunStatementAtCursor};
use crate::views::result_panel::{
    MAX_INSERT_COLUMNS, ResultPanel, ResultState, RowSearchConversionStatus, RowSearchMode,
};

impl Render for QueryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 把异步任务挂起的 toast 推送出来（如生产模式只读拦截提示）
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let secondary_bg = theme.secondary;
        let bg = theme.background;
        let accent = theme.accent;
        let danger = theme.danger;

        let running = self.running;
        let has_connection = self.connection.is_some();

        // 仅"执行中"状态在工具条显示实时耗时，其他状态由结果面板底部 status_bar 展示
        let running_elapsed = self.query_start.map(|t| t.elapsed()).map(format_elapsed);
        let (result_summary, has_result): (Option<String>, bool) =
            match self.result.read(cx).state() {
                ResultState::Ok(qr) => (None, !qr.rows.is_empty()),
                ResultState::Error(_) | ResultState::Released(_) => (None, false),
                ResultState::Running => (
                    Some(match &running_elapsed {
                        Some(s) => format!("执行中 {s}"),
                        None => "执行中".to_string(),
                    }),
                    false,
                ),
                ResultState::Empty => (None, false),
            };
        let panel_for_btn = self.result.read(cx);
        let has_selected =
            !panel_for_btn.selected_rows().is_empty() || panel_for_btn.selected_cell().is_some();
        // 写入口共用单表、视图、定位键和只读校验。
        let insert_reason = panel_for_btn.insert_block_reason();
        let modify_reason = panel_for_btn.modify_block_reason();
        let has_pending_insert = panel_for_btn.pending_insert().is_some();
        let _ = panel_for_btn;
        let is_production = self.connection.as_ref().is_some_and(|c| c.production);
        let warning = theme.warning;

        v_flex()
            .size_full()
            .bg(bg)
            .key_context("QueryTab")
            .on_action(cx.listener(|this, _: &RunQuery, window, cx| {
                this.handle_run(window, cx);
            }))
            .on_action(cx.listener(|this, _: &RunStatementAtCursor, window, cx| {
                this.handle_run_at_cursor(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FormatSql, window, cx| {
                this.handle_format(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExplainQuery, window, cx| {
                this.handle_explain(window, cx);
            }))
            .when(self.show_editor, |this| {
                this.child(
                    div()
                        .h(px(220.0))
                        .flex_none()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            Input::new(&self.editor)
                                .h_full()
                                .bordered(false)
                                .focus_bordered(false),
                        ),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(border)
                    .bg(secondary_bg)
                    .child({
                        let col_input = self.result.read(cx).column_filter_entity().clone();
                        let row_input = self.result.read(cx).row_filter_entity().clone();
                        let row_search_mode = self.result.read(cx).row_search_mode();
                        let row_search_status = self
                            .result
                            .read(cx)
                            .row_search_conversion_status(cx);
                        let row_filter_has_value = !row_input.read(cx).value().is_empty();
                        let id_conversion_ready = ramag_ui::database_search_settings(cx).is_ready();
                        let result_for_row_mode = self.result.clone();
                        let col_for_up = col_input.clone();
                        let col_for_down = col_input.clone();
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .on_action(
                                        move |action: &gpui_component::input::MoveUp,
                                              window,
                                              app| {
                                            col_for_up.update(app, |state, cx| {
                                                state.handle_action_for_context_menu(
                                                    Box::new(action.clone()),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        },
                                    )
                                    .on_action(
                                        move |action: &gpui_component::input::MoveDown,
                                              window,
                                              app| {
                                            col_for_down.update(app, |state, cx| {
                                                state.handle_action_for_context_menu(
                                                    Box::new(action.clone()),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        },
                                    )
                                    .child(
                                        ramag_ui::cleanable_input(
                                            &col_input,
                                            "sql-column-filter-clear",
                                            false,
                                            cx,
                                        )
                                            .small()
                                            .bordered(false)
                                            .focus_bordered(false),
                                    ),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&row_input)
                                        .small()
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .when(id_conversion_ready, |input| {
                                            input.prefix(row_search_mode_button(
                                                row_search_mode,
                                                result_for_row_mode,
                                                accent,
                                            ))
                                        })
                                        .when(row_filter_has_value, |input| {
                                            input.suffix(row_search_input_suffix(
                                                row_input,
                                                row_search_status,
                                                accent,
                                                muted_fg,
                                                danger,
                                            ))
                                        }),
                                ),
                            )
                    })
                    // 生产只读徽标：常驻工具条，与连接 Tab 徽标、写入口禁用同一语义
                    .when(is_production, |this| {
                        let mut chip_bg = warning;
                        chip_bg.a = 0.15;
                        this.child(
                            div()
                                .flex_none()
                                .px(px(6.0))
                                .py(px(1.0))
                                .rounded(px(4.0))
                                .bg(chip_bg)
                                .text_xs()
                                .text_color(warning)
                                .child("生产 · 只读"),
                        )
                    })
                    .when_some(result_summary, |this, summary| {
                        this.child(div().text_xs().text_color(muted_fg).child(summary))
                    })
                    .child({
                        let can_insert = insert_reason.is_none() && !has_pending_insert;
                        let insert_tip: Option<gpui::SharedString> =
                            match (insert_reason, has_pending_insert) {
                                (Some(reason), _) => Some(reason.into()),
                                (None, true) => Some("请先处理草稿".into()),
                                (None, false) => None,
                            };
                        ramag_ui::clickable_button("toolbar-insert")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .when_some(insert_tip, |button, tip| button.tooltip(tip))
                            .disabled(!can_insert)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                let Some(conn) = this.connection.clone() else {
                                    return;
                                };
                                let Some((schema, table)) = this.pinned_target.clone() else {
                                    return;
                                };
                                let svc = this.service.clone();
                                let panel = this.result.clone();
                                let handle = window.window_handle();
                                cx.spawn(async move |_, cx| {
                                    let cols = svc.list_columns(&conn, &schema, &table).await;
                                    let _ = cx.update_window(handle, |_, window, app| match cols {
                                        Ok(cols) => {
                                            if cols.len() > MAX_INSERT_COLUMNS {
                                                window.push_notification(
                                                    Notification::warning(format!(
                                                        "该表有 {} 列，超过行内新增的 {} 列上限；请使用 INSERT SQL",
                                                        cols.len(),
                                                        MAX_INSERT_COLUMNS
                                                    ))
                                                    .autohide(true),
                                                    app,
                                                );
                                                return;
                                            }
                                            let inputs: Vec<Entity<InputState>> = cols
                                                .iter()
                                                .map(|col| {
                                                    let placeholder = format!(
                                                        "{} · {}",
                                                        col.data_type.raw_type,
                                                        if col.nullable {
                                                            "可空"
                                                        } else {
                                                            "必填"
                                                        }
                                                    );
                                                    app.new(|cx_inner| {
                                                        InputState::new(window, cx_inner)
                                                            .validate(|value, _| {
                                                                value.len()
                                                                    <= MAX_SQL_QUERY_BYTES
                                                            })
                                                            .placeholder(placeholder)
                                                    })
                                                })
                                                .collect();
                                            let first_input = inputs.first().cloned();
                                            panel.update(app, |r, cx| {
                                                r.start_insert(cols, inputs, cx);
                                            });
                                            if let Some(input) = first_input {
                                                input.update(app, |state, cx_inner| {
                                                    state.focus(window, cx_inner);
                                                });
                                            }
                                        }
                                        Err(e) => {
                                            window.push_notification(
                                                Notification::error(format!("拉取表结构失败：{e}"))
                                                    .autohide(true),
                                                app,
                                            );
                                        }
                                    });
                                })
                                .detach();
                            }))
                    })
                    .child(
                        ramag_ui::clickable_button("toolbar-delete")
                            .ghost()
                            .small()
                            .icon(IconName::Minus)
                            .when_some(
                                match (modify_reason, has_selected) {
                                    (Some(reason), _) => Some(gpui::SharedString::from(reason)),
                                    (None, false) => Some("请先选择数据".into()),
                                    (None, true) => None,
                                },
                                |button, tip| button.tooltip(tip),
                            )
                            .disabled(!has_selected || modify_reason.is_some())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                let panel_ref = this.result.read(cx);
                                let multi = panel_ref.delete_preview_multi(cx);
                                let single = if multi.is_none() {
                                    panel_ref.delete_preview(cx)
                                } else {
                                    None
                                };
                                let _ = panel_ref;
                                if let Some((indices, _)) = &multi
                                    && !this.result.update(cx, |panel, cx| {
                                        panel.guard_batch_delete_count(indices.len(), cx)
                                    })
                                {
                                    return;
                                }
                                let result = this.result.clone();
                                let (title, preview, on_ok_indices, on_ok_single): (
                                    &'static str,
                                    String,
                                    Option<Vec<usize>>,
                                    Option<usize>,
                                ) = match (multi, single) {
                                    (Some((ids, summary)), _) => {
                                        ("删除选中行？", summary, Some(ids), None)
                                    }
                                    (None, Some((ri, p))) => {
                                        ("删除此行？", format!("将删除：{p}"), None, Some(ri))
                                    }
                                    _ => return,
                                };
                                window.open_dialog(cx, move |dialog, _, _| {
                                    let result_btn = result.clone();
                                    let preview_for_content = preview.clone();
                                    let on_ok_indices = on_ok_indices.clone();
                                    let on_ok_single = on_ok_single;
                                    let cancel = ramag_ui::clickable_button("del-row-cancel")
                                        .ghost()
                                        .small()
                                        .label("取消")
                                        .on_click(|_: &ClickEvent, window, app| {
                                            window.close_dialog(app);
                                        });
                                    let ok = ramag_ui::clickable_button("del-row-ok")
                                        .danger()
                                        .small()
                                        .label("删除")
                                        .on_click({
                                            let result = result_btn.clone();
                                            let indices = on_ok_indices.clone();
                                            let single = on_ok_single;
                                            move |_: &ClickEvent, window, app| {
                                                let started = result.update(app, |r, cx| {
                                                    if let Some(ids) = indices.clone() {
                                                        r.execute_delete_rows_async(ids, cx)
                                                    } else if let Some(ri) = single {
                                                        r.execute_delete_row_async(ri, cx)
                                                    } else {
                                                        false
                                                    }
                                                });
                                                if started {
                                                    window.close_dialog(app);
                                                }
                                            }
                                    });
                                    dialog
                                        .title(ramag_ui::closable_dialog_title(
                                            "delete-row-close",
                                            title,
                                            |_, _| {},
                                        ))
                                        .close_button(false)
                                        .width(px(520.0))
                                        .margin_top(px(180.0))
                                        .content(move |c, _, cx| {
                                            let muted_fg = cx.theme().muted_foreground;
                                            let p = preview_for_content.clone();
                                            c.child(div().text_sm().text_color(muted_fg).child(p))
                                        })
                                        .footer(
                                            h_flex()
                                                .w_full()
                                                .items_center()
                                                .justify_end()
                                                .gap(px(8.0))
                                                .child(cancel)
                                                .child(ok),
                                        )
                                });
                            })),
                    )
                    .child(
                        // 与导出配对：仅表树打开的单表结果可导入（pinned 表即目标）
                        ramag_ui::clickable_button("import-btn")
                            .ghost()
                            .small()
                            .icon(ramag_ui::icons::download())
                            .when(self.pinned_target.is_none(), |button| {
                                button.tooltip("请先打开表")
                            })
                            .disabled(self.pinned_target.is_none())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_table_import_dialog(window, cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("export-btn")
                            .ghost()
                            .small()
                            .icon(ramag_ui::icons::upload())
                            .disabled(!has_result)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.result.update(cx, |r, cx| r.export(cx));
                            })),
                    )
                    .when(running, |this| {
                        this.child(
                            ramag_ui::clickable_button("cancel-query")
                                .danger()
                                .small()
                                .icon(IconName::Close)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.handle_cancel(window, cx);
                                })),
                        )
                    })
                    .when(!running, |this| {
                        this.child(
                            ramag_ui::clickable_button("run-query")
                                .primary()
                                .small()
                                .icon(IconName::Play)
                                .disabled(!has_connection)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.handle_run(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.result.clone()),
            )
    }
}

fn row_search_mode_button(
    current: RowSearchMode,
    result: Entity<ResultPanel>,
    accent: gpui::Hsla,
) -> impl IntoElement {
    ramag_ui::clickable_button("sql-row-search-mode")
        .text()
        .small()
        .label(current.label())
        .dropdown_caret(true)
        .text_color(accent)
        .tooltip(match current {
            RowSearchMode::Normal => "@TEXT：按单元格展示文本包含搜索",
            RowSearchMode::Id => "@ID：外部转换后精确匹配整数单元格",
        })
        .pointer_dropdown_menu(move |mut menu, _, _| {
            for mode in [RowSearchMode::Normal, RowSearchMode::Id] {
                let label = if mode == current {
                    format!("✓ {}", mode.label())
                } else {
                    format!("  {}", mode.label())
                };
                let result = result.clone();
                menu = menu.item(ramag_ui::menu_item(label).on_click(
                    move |_: &ClickEvent, _, app| {
                        result.update(app, |panel, cx| {
                            panel.set_row_search_mode(mode, cx);
                        });
                    },
                ));
            }
            menu
        })
}

fn row_search_input_suffix(
    input: Entity<InputState>,
    status: Option<RowSearchConversionStatus>,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .when_some(status, |suffix, status| {
            suffix.child(row_search_conversion_label(status, accent, muted, danger))
        })
        .child(
            ramag_ui::clickable_button("sql-row-filter-clear")
                .icon(IconName::CircleX)
                .ghost()
                .xsmall()
                .tab_stop(false)
                .text_color(muted)
                .on_click(move |_, window, cx| {
                    input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                }),
        )
}

fn row_search_conversion_label(
    status: RowSearchConversionStatus,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> gpui::AnyElement {
    let (label, color) = match status {
        RowSearchConversionStatus::Converting => ("→ 转换中…".to_string(), muted),
        RowSearchConversionStatus::Ready(id) => (format!("→ {id}"), accent),
        RowSearchConversionStatus::Error(_) => ("→ 转换失败".to_string(), danger),
    };

    div()
        .flex_none()
        .text_xs()
        .text_color(color)
        .child(label)
        .into_any_element()
}
