use super::pagination::parse_result_page;
use super::states::{render_affected_result, render_row_search_blocker};
use super::*;
use gpui_component::IconName;

use crate::views::result_value::display_cell_value;
/// 构建 SQL 结果表：复用虚拟行列表，并把宽列内容交给可拖拽的横向滚动条浏览。
#[allow(clippy::too_many_arguments)]
pub(in crate::views) fn render_table(
    panel: &mut ResultPanel,
    result: &Arc<QueryResult>,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    secondary_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted_bg: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let columns = &result.columns;
    let column_types = &result.column_types;
    let total_rows = result.rows.len();
    let affected = result.affected_rows;
    let elapsed = result.elapsed_ms;
    let pagination = panel.pagination();
    let row_number_offset = pagination
        .map(|pagination| pagination.page.saturating_mul(pagination.page_size))
        .unwrap_or(0);

    if columns.is_empty() {
        return render_affected_result(affected, elapsed, fg, muted_fg);
    }

    if let Some(blocker) = panel.row_search_blocker(cx) {
        return render_row_search_blocker(blocker, muted_fg, cx.theme().danger);
    }

    // 排序、筛选与列宽估算可能扫描大结果集，统一在受限工作池构建。
    let Some(view) = ensure_display_view(panel, result, cx) else {
        let error = panel.display_view_error.clone();
        let message = error.clone().unwrap_or_else(|| {
            pagination.map_or_else(
                || {
                    format!(
                        "正在准备结果视图…（最多处理 {} 行）",
                        total_rows.min(MAX_ROWS_DISPLAY)
                    )
                },
                |pagination| format!("正在准备第 {} 页…", pagination.page + 1),
            )
        });
        let mut placeholder = v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_xs()
            .text_color(if error.is_some() {
                cx.theme().danger
            } else {
                muted_fg
            })
            .child(message);
        if error.is_some() {
            placeholder = placeholder.child(
                ramag_ui::clickable_button("result-view-retry")
                    .ghost()
                    .small()
                    .label("重试")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.invalidate_display_view();
                        cx.notify();
                    })),
            );
        }
        return placeholder.into_any_element();
    };
    let DisplayView {
        visible_col_indices,
        matched_col_count,
        columns_truncated,
        display_indices,
        default_col_widths,
        right_align,
        truncated,
        cols_filtered,
        row_filtering,
        pre_filter_count,
    } = view;
    let total_cols = columns.len();
    let visible_cols_count = visible_col_indices.len();
    let visible_count = display_indices.len();

    let col_widths: Vec<gpui::Pixels> = default_col_widths
        .iter()
        .enumerate()
        .map(|(ci, &default_width)| panel.col_width_override(ci).unwrap_or(default_width))
        .collect();
    let display_binary_16_as_uuid =
        ramag_ui::database_result_settings(cx).display_binary_16_as_uuid;
    let last_row_number = row_number_offset.saturating_add(total_rows);
    let row_num_width =
        px((last_row_number.to_string().len() as f32 * 9.0 + 16.0).clamp(40.0, 70.0));
    let checkbox_col_width = px(32.0);
    let total_content_width = visible_col_indices
        .iter()
        .map(|&ci| col_widths[ci])
        .fold(row_num_width + checkbox_col_width, |acc, w| acc + w);

    let mono_font = cx.theme().mono_font_family.clone();
    let current_sort = panel.sort_by();
    let header_cells: Vec<AnyElement> = visible_col_indices
        .iter()
        .map(|&ci| {
            render_header_cell(
                ci,
                columns,
                column_types,
                &col_widths,
                current_sort,
                fg,
                muted_fg,
                border,
                cx,
            )
        })
        .collect();

    let row_num_header = div()
        .w(row_num_width)
        .flex_none()
        .px_2()
        .border_r_1()
        .border_color(border)
        .into_any_element();

    let visible_row_indices = display_indices.clone();
    let (visible_selected, all_selected) = panel.visible_selection_summary(&visible_row_indices);
    let selected_rows_set = panel.selected_rows();
    let panel_entity = cx.entity();

    let checkbox_header = {
        let panel = panel_entity.clone();
        let visible_row_indices = visible_row_indices.clone();
        div()
            .w(checkbox_col_width)
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .items_center()
                    .justify_center()
                    .child(
                        ramag_ui::clickable_checkbox("rows-toggle-all")
                            .checked(all_selected)
                            .on_click(move |_: &bool, _, app| {
                                panel.update(app, |this, cx| {
                                    this.toggle_visible_rows(&visible_row_indices, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    };

    let header = h_flex()
        .w(total_content_width)
        .h(px(34.0))
        .flex_none()
        .items_center()
        .bg(secondary_bg)
        .border_b_1()
        .border_color(border)
        .child(checkbox_header)
        .child(row_num_header)
        .children(header_cells);

    let frame = Rc::new(TableRowFrame {
        result: result.clone(),
        display_indices,
        visible_col_indices: visible_col_indices.clone(),
        col_widths: col_widths.clone(),
        display_binary_16_as_uuid,
        right_align,
        row_number_offset,
        row_num_width,
        checkbox_col_width,
        total_content_width,
        mono_font,
        fg,
        muted_fg,
        border,
        muted_bg,
        accent,
    });

    let has_pending_insert = panel.pending_insert().is_some();
    let pending_edit_count = panel.pending_cell_edit_count();
    let dml_busy = panel.dml_busy();
    let row_count = frame.display_indices.len() + if has_pending_insert { 1 } else { 0 };

    let frame_for_rows = frame.clone();
    let body = uniform_list(
        "result-rows",
        row_count,
        cx.processor(move |this, range: Range<usize>, _w, cx| {
            range
                .map(|i| {
                    if i < frame_for_rows.display_indices.len() {
                        render_data_row(this, &frame_for_rows, i, cx)
                    } else {
                        render_pending_row(this, &frame_for_rows, cx)
                    }
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(panel.uniform_scroll())
    .w(frame.total_content_width)
    .flex_1()
    .restrict_scroll_to_axis();

    let selected_info: Option<String> = panel.selected_cell().and_then(|(ri, ci)| {
        let col_name = columns.get(ci)?.clone();
        let val = panel.cell_value(ri, ci)?;
        let preview = display_cell_value(Some(val), 40, display_binary_16_as_uuid);
        let hidden_note = if visible_row_indices.contains(&ri) {
            ""
        } else {
            "（当前隐藏）"
        };
        Some(format!(
            "· [{}, {}] = {}{hidden_note}",
            row_number_offset.saturating_add(ri).saturating_add(1),
            col_name,
            preview
        ))
    });
    let selected_count = selected_rows_set.len();
    let hidden_selected = selected_count.saturating_sub(visible_selected);
    let selected_scope = (selected_count > 0).then(|| {
        if hidden_selected > 0 {
            format!("· 已选 {selected_count} 行，其中 {hidden_selected} 行当前隐藏")
        } else {
            format!("· 已选 {selected_count} 行")
        }
    });

    let mut status_parts = Vec::with_capacity(4);
    if pending_edit_count > 0 {
        status_parts.push(format!("未提交 {pending_edit_count} 项修改"));
    }
    if let Some((mode, output)) = panel.converted_row_search(cx) {
        status_parts.push(format!("{} → {}", mode.label(), output.display_preview(80)));
    }
    if cols_filtered {
        if columns_truncated {
            status_parts.push(format!(
                "命中 {matched_col_count} / {total_cols} 列（仅显示前 {visible_cols_count} 列）"
            ));
        } else {
            status_parts.push(format!("命中 {matched_col_count} / {total_cols} 列"));
        }
    } else if columns_truncated {
        status_parts.push(format!(
            "显示 {visible_cols_count} / {total_cols} 列（已截断）"
        ));
    }
    let pagination_ui = pagination;
    let total_summary: Option<String> = pagination_ui.and_then(|p| match p.total {
        TotalRows::Counting => Some("总行数计算中…".to_string()),
        TotalRows::Known(n) => Some(format!("共 {n} 行")),
        TotalRows::Unavailable => None,
    });
    let total_pages: Option<u64> = pagination_ui.and_then(|p| match p.total {
        TotalRows::Known(n) if p.page_size > 0 => Some(n.div_ceil(p.page_size as u64).max(1)),
        _ => None,
    });
    if row_filtering {
        if pagination_ui.is_some() {
            let range_start = row_number_offset.saturating_add(1);
            let range_end = row_number_offset.saturating_add(total_rows);
            status_parts.push(format!(
                "当前页命中 {visible_count} / {pre_filter_count} 行（数据库范围 {range_start}-{range_end}）"
            ));
        } else if truncated {
            status_parts.push(format!(
                "命中 {visible_count} / {pre_filter_count} 行（仅搜索前 {pre_filter_count} / {total_rows} 行）"
            ));
        } else {
            status_parts.push(format!("命中 {visible_count} / {pre_filter_count} 行"));
        }
    } else if pagination_ui.is_some() {
        if total_rows == 0 {
            status_parts.push("当前页 0 行".to_string());
        } else {
            let range_start = row_number_offset.saturating_add(1);
            let range_end = row_number_offset.saturating_add(total_rows);
            status_parts.push(format!("显示 {range_start}-{range_end} 行"));
        }
        if let Some(total_text) = total_summary {
            status_parts.push(total_text);
        }
    } else if truncated {
        status_parts.push(format!(
            "显示 {pre_filter_count} / {total_rows} 行（已截断）"
        ));
    } else {
        status_parts.push(format!("{total_rows} 行"));
    }
    let status_summary = status_parts.join(" · ");

    let status_context = h_flex()
        .id("result-status-context")
        .debug_selector(|| "result-status-context".into())
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(status_summary),
        )
        .child(div().flex_none().child(format!("· 耗时 {elapsed} ms")))
        .when_some(selected_scope, |this, scope| {
            this.child(div().flex_none().child(scope))
        })
        .when_some(selected_info, |this, info| {
            this.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(info),
            )
        });

    let status_bar = h_flex()
        .id("result-status-bar")
        .debug_selector(|| "result-status-bar".into())
        .w_full()
        .min_w_0()
        .flex_none()
        .flex_wrap()
        .items_center()
        .px_3()
        .py_1()
        .gap_2()
        .border_t_1()
        .border_color(border)
        .bg(secondary_bg)
        .text_xs()
        .text_color(muted_fg)
        .child(status_context)
        .when_some(pagination_ui, |this, pagination| {
            let has_previous_page = pagination.page > 0;
            let previous_page = pagination.page.saturating_sub(1);
            let next_page = pagination.page.saturating_add(1);
            let panel_for_previous = panel_entity.clone();
            let panel_for_next = panel_entity.clone();
            this.child(render_page_size_selector(
                pagination.page_size,
                panel_entity.clone(),
                pending_edit_count > 0 || dml_busy,
            ))
            .child(
                ramag_ui::clickable_button("result-page-previous")
                    .debug_selector(|| "result-page-previous".into())
                    .ghost()
                    .small()
                    .label("上页")
                    .disabled(!has_previous_page || pending_edit_count > 0 || dml_busy)
                    .on_click(move |_, _, app| {
                        panel_for_previous.update(app, |_, cx| {
                            cx.emit(ResultPanelEvent::PageRequested(previous_page));
                        });
                    }),
            )
            .child(div().flex_none().child(match total_pages {
                Some(pages) => format!("第 {} / {} 页", pagination.page + 1, pages),
                None => format!("第 {} 页", pagination.page + 1),
            }))
            .when_some(
                total_pages.filter(|pages| *pages > 1),
                |this, total_pages| {
                    let panel_for_jump = panel_entity.clone();
                    let current_page = pagination.page.saturating_add(1);
                    this.child(
                        ramag_ui::clickable_button("result-page-jump")
                            .debug_selector(|| "result-page-jump".into())
                            .ghost()
                            .small()
                            .label("跳页")
                            .tooltip(format!("输入 1-{total_pages} 的页码"))
                            .disabled(pending_edit_count > 0 || dml_busy)
                            .on_click(move |_, window, app| {
                                let panel = panel_for_jump.clone();
                                ramag_ui::open_bounded_prompt(
                                    "跳转到结果页",
                                    format!("输入 1-{total_pages} 的页码"),
                                    &current_page.to_string(),
                                    "跳转",
                                    32,
                                    move |value, _, app| match parse_result_page(
                                        &value,
                                        total_pages,
                                    ) {
                                        Ok(page) => panel.update(app, |_, cx| {
                                            cx.emit(ResultPanelEvent::PageRequested(page));
                                        }),
                                        Err(message) => panel.update(app, |panel, cx| {
                                            panel.notify_result_error(message, cx);
                                        }),
                                    },
                                    window,
                                    app,
                                );
                            }),
                    )
                },
            )
            .child(
                ramag_ui::clickable_button("result-page-next")
                    .debug_selector(|| "result-page-next".into())
                    .ghost()
                    .small()
                    .label("下页")
                    .tooltip("无排序时顺序不固定")
                    .disabled(!pagination.has_more || pending_edit_count > 0 || dml_busy)
                    .on_click(move |_, _, app| {
                        panel_for_next.update(app, |_, cx| {
                            cx.emit(ResultPanelEvent::PageRequested(next_page));
                        });
                    }),
            )
        })
        .when(has_pending_insert, |this| {
            let panel_for_cancel = panel_entity.clone();
            let panel_for_submit = panel_entity.clone();
            this.child(
                ramag_ui::clickable_button("insert-cancel-bar")
                    .ghost()
                    .small()
                    .label("取消")
                    .disabled(dml_busy)
                    .on_click(move |_, _, app| {
                        panel_for_cancel.update(app, |r, cx| r.cancel_insert(cx));
                    }),
            )
            .child(
                ramag_ui::clickable_button("insert-submit-bar")
                    .primary()
                    .small()
                    .label(if dml_busy { "提交中…" } else { "提交" })
                    .disabled(dml_busy)
                    .on_click(move |_, _, app| {
                        panel_for_submit.update(app, |r, cx| r.submit_insert(cx));
                    }),
            )
        })
        .when(pending_edit_count > 0, |this| {
            let panel_for_cancel = panel_entity.clone();
            let panel_for_submit = panel_entity.clone();
            this.child(
                ramag_ui::clickable_button("cell-edits-cancel-bar")
                    .ghost()
                    .small()
                    .icon(IconName::Undo2)
                    .label("撤销修改")
                    .tooltip("撤销当前结果中的未提交单元格修改")
                    .disabled(dml_busy)
                    .on_click(move |_, _, app| {
                        panel_for_cancel.update(app, |panel, cx| {
                            panel.clear_pending_cell_edits(cx);
                        });
                    }),
            )
            .child(
                ramag_ui::clickable_button("cell-edits-submit-bar")
                    .primary()
                    .small()
                    .icon(IconName::Check)
                    .label(if dml_busy {
                        "提交中…"
                    } else {
                        "提交修改"
                    })
                    .tooltip("按行提交当前结果中的未提交单元格修改")
                    .disabled(dml_busy)
                    .on_click(move |_, _, app| {
                        panel_for_submit.update(app, |panel, cx| {
                            panel.commit_pending_cell_edits_async(cx);
                        });
                    }),
            )
        });

    // 外层横向滚动，虚拟列表纵向滚动；滚动条使用固定底部布局行，避免被结果内容覆盖。
    let table_view = div()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            div()
                .id("result-h-scroll")
                .debug_selector(|| "result-h-scroll".into())
                .size_full()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(panel.h_scroll())
                .child(
                    v_flex()
                        .w(frame.total_content_width)
                        .h_full()
                        .child(header)
                        .child(body),
                ),
        )
        .child(
            div()
                .id("result-scroll-input")
                .absolute()
                .inset_0()
                .on_scroll_wheel(cx.listener(ResultPanel::on_result_scroll)),
        )
        .child(
            div()
                .id("result-v-scrollbar")
                .debug_selector(|| "result-v-scrollbar".into())
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(16.0))
                .bg(cx.theme().scrollbar)
                .child(
                    Scrollbar::vertical(panel.uniform_scroll())
                        .id("result-v-scrollbar-control")
                        .scrollbar_show(ScrollbarShow::Always),
                ),
        );

    let show_horizontal_scrollbar =
        ramag_ui::database_result_settings(cx).show_horizontal_scrollbar;
    let horizontal_scrollbar = div()
        .id("result-h-scrollbar")
        .debug_selector(|| "result-h-scrollbar".into())
        .flex_none()
        .w_full()
        .h(px(16.0))
        .relative()
        .bg(cx.theme().scrollbar)
        .child(
            Scrollbar::horizontal(panel.h_scroll())
                .id("result-h-scrollbar-control")
                .scroll_size(gpui::size(frame.total_content_width, px(16.0)))
                .scrollbar_show(ScrollbarShow::Always),
        );

    let table_container = v_flex()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(table_view)
        .when(show_horizontal_scrollbar, |container| {
            container.child(horizontal_scrollbar)
        });

    v_flex()
        .size_full()
        .min_w_0()
        .child(table_container)
        .child(status_bar)
        .into_any_element()
}
