use super::*;

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
        return v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .text_color(fg)
                    .child(format!("✓ {affected} 行受影响")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("{elapsed} ms")),
            )
            .into_any_element();
    }

    if let Some(blocker) = panel.row_search_blocker(cx) {
        let (message, color) = match blocker {
            RowSearchBlocker::Converting => ("正在通过外部程序转换 ID…".to_string(), muted_fg),
            RowSearchBlocker::Error(error) => (format!("ID 转换失败：{error}"), cx.theme().danger),
        };
        return v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .text_xs()
            .text_color(color)
            .child(message)
            .child(
                div()
                    .text_color(muted_fg)
                    .child("请修改搜索词，或检查设置中的转换方式。"),
            )
            .into_any_element();
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

    let has_pending = panel.pending_insert().is_some();
    let dml_busy = panel.dml_busy();
    let row_count = frame.display_indices.len() + if has_pending { 1 } else { 0 };

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
        let val = result.rows.get(ri)?.values.get(ci)?;
        let preview = val.display_preview(40);
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

    let mut status_parts = Vec::with_capacity(3);
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
    let pagination_ui = pagination.filter(|pagination| pagination.page > 0 || pagination.has_more);
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

    let status_bar = h_flex()
        .w_full()
        .flex_none()
        .items_center()
        .px_3()
        .py_1()
        .gap_2()
        .border_t_1()
        .border_color(border)
        .bg(secondary_bg)
        .text_xs()
        .text_color(muted_fg)
        .child(div().child(status_summary))
        .child(div().child(format!("· 耗时 {elapsed} ms")))
        .when_some(selected_scope, |this, scope| this.child(div().child(scope)))
        .when_some(selected_info, |this, info| {
            this.child(div().overflow_hidden().text_ellipsis().child(info))
        })
        .child(div().flex_1())
        .when_some(pagination_ui, |this, pagination| {
            let has_previous_page = pagination.page > 0;
            let previous_page = pagination.page.saturating_sub(1);
            let next_page = pagination.page.saturating_add(1);
            let panel_for_previous = panel_entity.clone();
            let panel_for_next = panel_entity.clone();
            this.child(
                ramag_ui::clickable_button("result-page-previous")
                    .ghost()
                    .small()
                    .label("上页")
                    .disabled(!has_previous_page)
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
            .child(
                ramag_ui::clickable_button("result-page-next")
                    .ghost()
                    .small()
                    .label("下页")
                    .tooltip("下一页；未写 ORDER BY 时数据库不保证分页顺序")
                    .disabled(!pagination.has_more)
                    .on_click(move |_, _, app| {
                        panel_for_next.update(app, |_, cx| {
                            cx.emit(ResultPanelEvent::PageRequested(next_page));
                        });
                    }),
            )
        })
        .when(has_pending, |this| {
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
        });

    // 外层横向滚动，虚拟列表纵向滚动；透明输入层统一锁定一次手势的主轴。
    v_flex()
        .size_full()
        .min_w_0()
        .child(
            div()
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
                ),
        )
        .child(status_bar)
        .into_any_element()
}
