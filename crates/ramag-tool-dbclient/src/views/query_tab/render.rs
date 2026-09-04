//! 查询标签渲染。

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

use super::QueryTab;
use super::comparison_toolbar::result_comparison_menu;
use super::render_helpers::{
    TransactionSavepointState, result_view_tabs, row_filter_prefix, row_search_input_suffix,
    transaction_savepoint_controls,
};
use super::sql_utils::format_elapsed;
use super::toolbar::render_delete_button;
use super::transaction::MAX_TRANSACTION_SAVEPOINTS;

use crate::actions::{ExplainQuery, FormatSql, RunQuery, RunStatementAtCursor};
use crate::views::result_panel::{MAX_INSERT_COLUMNS, ResultState};

impl Render for QueryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let transaction_active = self.transaction.is_some();
        let transaction_dirty = self.transaction_is_dirty();
        let transaction_savepoints = self.transaction_savepoints();
        let latest_savepoint = transaction_savepoints
            .last()
            .map(|savepoint| savepoint.name.clone());
        let transaction_label = self.transaction_label();
        let transaction_error = self.transaction_error.is_some();
        let result_entity = self.active_result();
        let data_result_entity = self.result.clone();
        let (can_capture_comparison, has_comparison_baseline) = {
            let data_result_panel = data_result_entity.read(cx);
            (
                data_result_panel.can_capture_comparison_baseline(),
                data_result_panel.has_comparison_baseline(),
            )
        };
        let plan_visible = self.show_plan;
        let can_cross_connection_compare = !plan_visible && self.can_compare_cross_connection(cx);
        let plan_available = !self.plan_result_is_empty(cx);

        // 仅"执行中"状态在工具条显示实时耗时，其他状态由结果面板底部 status_bar 展示
        let running_elapsed = self.query_start.map(|t| t.elapsed()).map(format_elapsed);
        let (result_summary, has_result): (Option<String>, bool) =
            match result_entity.read(cx).state() {
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
        let pending_cell_edit_count = self.result.read(cx).pending_cell_edit_count();
        let panel_for_btn = result_entity.read(cx);
        let has_selected =
            !panel_for_btn.selected_rows().is_empty() || panel_for_btn.selected_cell().is_some();
        // 写入口共用单表、视图、定位键和只读校验。
        let insert_reason = panel_for_btn.insert_block_reason();
        let has_pending_insert = panel_for_btn.pending_insert().is_some();
        let modify_reason = if pending_cell_edit_count > 0 {
            Some("请先提交或撤销未提交单元格修改")
        } else {
            panel_for_btn.modify_block_reason()
        };
        let dml_busy = panel_for_btn.dml_busy();
        let _ = panel_for_btn;
        let is_production = self.connection.as_ref().is_some_and(|c| c.production);
        let warning = theme.warning;
        let query_tab_entity = cx.entity();
        let transaction_controls = if transaction_active {
            h_flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(
                    ramag_ui::clickable_button("transaction-commit")
                        .primary()
                        .small()
                        .icon(IconName::Check)
                        .label("提交")
                        .tooltip("提交当前事务")
                        .disabled(
                            self.transaction_busy
                                || running
                                || dml_busy
                                || pending_cell_edit_count > 0,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.finish_transaction(true, cx);
                        })),
                )
                .child(
                    ramag_ui::clickable_button("transaction-rollback")
                        .ghost()
                        .small()
                        .icon(IconName::Undo2)
                        .label("回滚")
                        .tooltip("回滚当前事务")
                        .disabled(
                            self.transaction_busy
                                || running
                                || dml_busy
                                || pending_cell_edit_count > 0,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.finish_transaction(false, cx);
                        })),
                )
                .child(transaction_savepoint_controls(
                    query_tab_entity.clone(),
                    TransactionSavepointState {
                        transaction_busy: self.transaction_busy,
                        running,
                        dml_busy,
                        pending_cell_edits: pending_cell_edit_count > 0,
                        savepoint_count: transaction_savepoints.len(),
                        latest_savepoint,
                        max_savepoints: MAX_TRANSACTION_SAVEPOINTS,
                    },
                    muted_fg,
                ))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(if transaction_error {
                            danger
                        } else if transaction_dirty {
                            warning
                        } else {
                            muted_fg
                        })
                        .child(if transaction_error {
                            "请回滚或重试"
                        } else if transaction_dirty {
                            "未提交"
                        } else {
                            "已开启"
                        }),
                )
                .into_any_element()
        } else {
            h_flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(
                    ramag_ui::clickable_button("transaction-begin")
                        .ghost()
                        .small()
                        .icon(IconName::Play)
                        .label("开始事务")
                        .tooltip(if dml_busy {
                            "上一写操作尚未完成，请稍候"
                        } else {
                            "开启手动提交事务"
                        })
                        .disabled(
                            !has_connection
                                || self.connection.as_ref().is_some_and(|connection| {
                                    !connection.driver.supports_transactions()
                                })
                                || self.transaction_busy
                                || running
                                || dml_busy
                                || pending_cell_edit_count > 0,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.begin_transaction(cx);
                        })),
                )
                .when(transaction_error, |this| {
                    this.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(danger)
                            .child("事务已结束，请重新开启"),
                    )
                })
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_w_0()
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
                ramag_ui::responsive_toolbar()
                    .debug_selector(|| "sql-result-toolbar".into())
                    .flex_none()
                    .gap_3()
                    .px_3()
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(border)
                    .bg(secondary_bg)
                    .child({
                        let col_input = result_entity.read(cx).column_filter_entity().clone();
                        let row_input = result_entity.read(cx).row_filter_entity().clone();
                        let row_search_mode = result_entity.read(cx).row_search_mode();
                        let row_search_status = result_entity
                            .read(cx)
                            .row_search_conversion_status(cx);
                        let row_filter_has_value = !row_input.read(cx).value().is_empty();
                        let id_conversion_ready =
                            ramag_ui::database_search_settings(cx).is_ready();
                        let result_for_row_mode = result_entity.clone();
                        let col_for_up = col_input.clone();
                        let col_for_down = col_input.clone();
                        h_flex()
                            .debug_selector(|| "sql-result-filter-group".into())
                            .flex_1()
                            // 保留两组筛选输入的可用宽度；空间不足时让后续操作换到下一行。
                            .min_w(px(220.0))
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
                                div()
                                    .flex_none()
                                    .w(px(1.0))
                                    .h(px(20.0))
                                    .bg(border),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&row_input)
                                        .small()
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .prefix(row_filter_prefix(
                                            row_search_mode,
                                            result_for_row_mode,
                                            accent,
                                            muted_fg,
                                            id_conversion_ready,
                                        ))
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
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted_fg)
                                    .child(transaction_label),
                            )
                            .child(transaction_controls),
                    )
                    .when_some(result_summary, |this, summary| {
                        this.child(div().text_xs().text_color(muted_fg).child(summary))
                    })
                    .child(result_comparison_menu(
                        query_tab_entity.clone(),
                        data_result_entity,
                        can_capture_comparison,
                        has_comparison_baseline,
                        can_cross_connection_compare,
                    ))
                    .child({
                        let can_insert = !plan_visible
                            && insert_reason.is_none()
                            && !has_pending_insert
                            && pending_cell_edit_count == 0;
                        let insert_tip: gpui::SharedString = if let Some(reason) = insert_reason {
                            reason.into()
                        } else if has_pending_insert {
                            "请先处理草稿".into()
                        } else if pending_cell_edit_count > 0 {
                            "请先提交或撤销未提交单元格修改".into()
                        } else {
                            "新增行".into()
                        };
                        ramag_ui::clickable_button("toolbar-insert")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip(insert_tip)
                            .disabled(!can_insert)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                let Some(conn) = this.connection.clone() else {
                                    return;
                                };
                                let Some((schema, table)) = this.pinned_target.clone() else {
                                    return;
                                };
                                let svc = this.service.clone();
                                let panel = this.active_result();
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
                    .child(render_delete_button(
                        plan_visible,
                        has_selected,
                        modify_reason,
                        cx,
                    ))
                    .child(
                        // 与导出配对：仅表树打开的单表结果可导入（pinned 表即目标）
                        ramag_ui::clickable_button("import-btn")
                            .ghost()
                            .small()
                            .icon(ramag_ui::icons::download())
                            .tooltip(if pending_cell_edit_count > 0 {
                                "请先提交或撤销未提交单元格修改"
                            } else if self.pinned_target.is_some() {
                                "导入数据"
                            } else {
                                "请先打开表"
                            })
                            .disabled(
                                plan_visible
                                    || self.pinned_target.is_none()
                                    || pending_cell_edit_count > 0,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_table_import_dialog(window, cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("export-btn")
                    .ghost()
                    .small()
                    .icon(ramag_ui::icons::upload())
                    .tooltip("导出")
                    .disabled(!has_result)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.active_result().update(cx, |r, cx| r.export(cx));
                            })),
                    )
                    .when(running && self.cancel_handle.is_some(), |this| {
                        this.child(
                            ramag_ui::clickable_button("cancel-query")
                    .danger()
                    .small()
                    .icon(IconName::Close)
                    .tooltip("取消")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.handle_cancel(window, cx);
                                })),
                        )
                    })
                    .when(!running, |this| {
                        this.child(
                            ramag_ui::clickable_button("run-query")
                                .debug_selector(|| "sql-run-query".into())
                    .primary()
                    .small()
                    .icon(IconName::Play)
                            .tooltip(if pending_cell_edit_count > 0 {
                                "请先提交或撤销未提交单元格修改"
                            } else if dml_busy {
                                "上一写操作尚未完成，请稍候"
                            } else {
                                "运行"
                            })
                        .disabled(
                            !has_connection
                                || self.transaction_busy
                                || dml_busy
                                || pending_cell_edit_count > 0,
                        )
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.handle_run(window, cx);
                                })),
                        )
                    }),
            )
            .child(result_view_tabs(
                query_tab_entity,
                plan_visible,
                plan_available,
                border,
                secondary_bg,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(result_entity),
            )
    }
}
