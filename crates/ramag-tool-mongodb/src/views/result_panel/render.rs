use super::*;

impl Render for ResultPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }
        if std::mem::take(&mut self.pending_close_dialog) {
            window.close_dialog(cx);
        }
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        if self.running {
            return v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(empty_hint("执行中…", muted))
                .into_any_element();
        }
        if let Some(err) = self.error.clone() {
            return v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(error_hint(err, danger))
                .into_any_element();
        }
        let Some((affected, elapsed, truncated)) = self
            .result
            .as_ref()
            .map(|r| (r.affected, r.elapsed_ms, r.truncated))
        else {
            return v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(empty_hint(
                    "（点击左侧 collection 自动开 Tab，或在编辑器写命令后运行）",
                    muted,
                ))
                .into_any_element();
        };
        let total_docs = self.docs_arc.as_ref().map_or(0, |docs| docs.len());
        let Some(table_arc) = self.table.clone() else {
            let hint = if self.table_building {
                format!("正在构建表格视图…（{total_docs} 条文档）")
            } else if affected > 0 {
                format!("已执行写操作，影响 {affected} 条")
            } else if self.is_drilled() {
                "（空）".to_string()
            } else {
                "（无文档返回）".to_string()
            };
            let mut root = v_flex().size_full().bg(bg).child(toolbar::render(self, cx));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root.child(empty_hint(hint, muted)).into_any_element();
        };

        if let Some(blocker) = self.row_search_blocker(cx) {
            let (hint, color) = match blocker {
                RowSearchBlocker::Converting => ("正在通过外部程序转换 ID…".to_string(), muted),
                RowSearchBlocker::Error(error) => (format!("ID 转换失败：{error}"), danger),
            };
            let mut root = v_flex().size_full().bg(bg).child(toolbar::render(self, cx));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root.child(empty_hint(hint, color)).into_any_element();
        }

        // 路径过滤可临时生成只读钻取视图。
        if let Some((flat_docs, flat_table, drill_path)) = self.try_drill_path(cx) {
            let n = flat_docs.len();
            let filters = self.parse_column_filter(cx).filters;
            let col_indices = column_indices_for(&flat_table, &filters);
            let row_filter = self.effective_row_filter(cx);
            let mut row_indices = row_indices_for_cancellable(&flat_table, &row_filter, None)
                .ok()
                .flatten()
                .unwrap_or_else(|| (0..flat_table.rows.len()).collect());
            if let Some((sort_path, dir)) = self.sort_by.clone()
                && let Some(ci) = flat_table.columns.iter().position(|c| c.path == sort_path)
            {
                let numeric = matches!(
                    flat_table.columns[ci].kind,
                    "int" | "long" | "double" | "decimal"
                );
                table::sort_row_indices(&flat_table, ci, numeric, dir, &mut row_indices);
            }
            let filtered_rows = row_indices.len();
            let row_indices = Arc::new(row_indices);
            let mut summary = if row_filter.is_active() {
                format!("钻取「{drill_path}」· 命中 {filtered_rows} / {n} 条")
            } else {
                format!("钻取「{drill_path}」· {n} 条")
            };
            if let Some((mode, output)) = self.converted_row_search(cx) {
                summary.insert_str(
                    0,
                    &format!("{} → {} · ", mode.label(), output.display_preview(80)),
                );
            }
            let mut root = v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(div().h(px(1.0)).bg(border))
                .child(flatten_hint(&drill_path, n, border, muted, bg));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root
                .child(div().flex_1().min_h_0().child(table::render(
                    self,
                    flat_table,
                    col_indices,
                    row_indices,
                    false,
                    Some(flat_docs),
                    cx,
                )))
                .child(render_status_bar(
                    summary,
                    border,
                    muted,
                    bg,
                    self.pagination,
                    cx.entity(),
                ))
                .into_any_element();
        }

        let col_indices = self.filtered_column_indices(cx);
        let Some((row_indices, rows_filtered)) = self.display_row_indices(cx) else {
            let (hint, hint_color) = if self.row_view_building {
                (format!("正在筛选 / 排序…（{total_docs} 行）"), muted)
            } else if let Some(error) = &self.row_view_error {
                (error.clone(), danger)
            } else {
                ("正在准备行视图…".to_string(), muted)
            };
            let mut root = v_flex().size_full().bg(bg).child(toolbar::render(self, cx));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root.child(empty_hint(hint, hint_color)).into_any_element();
        };
        let filtered_rows = row_indices.len();
        let visible_selected = if rows_filtered {
            row_indices
                .iter()
                .filter(|ri| self.selected_rows.contains(ri))
                .count()
        } else {
            self.selected_rows.len()
        };
        let hidden_selected = self.selected_rows.len().saturating_sub(visible_selected);
        let total_cols = self.table.as_ref().map(|t| t.columns.len()).unwrap_or(0);
        let discovered_cols = table_arc.total_columns;
        let visible_cols_count = col_indices.as_ref().map(|v| v.len()).unwrap_or(total_cols);
        let mut summary = match (rows_filtered, col_indices.is_some()) {
            (true, true) => format!(
                "命中 {visible_cols_count} / {total_cols} 列 · {filtered_rows} / {total_docs} 行 · 耗时 {elapsed}ms"
            ),
            (true, false) => format!("命中 {filtered_rows} / {total_docs} 行 · 耗时 {elapsed}ms"),
            (false, true) => format!(
                "命中 {visible_cols_count} / {total_cols} 列 · {total_docs} 行 · 耗时 {elapsed}ms"
            ),
            (false, false) => format!("{total_docs} 行 · 耗时 {elapsed}ms"),
        };
        if let Some((mode, output)) = self.converted_row_search(cx) {
            summary.insert_str(
                0,
                &format!("{} → {} · ", mode.label(), output.display_preview(80)),
            );
        }
        if !self.selected_rows.is_empty() {
            if hidden_selected > 0 {
                summary.push_str(&format!(
                    " · 已选 {} 行，其中 {hidden_selected} 行当前隐藏",
                    self.selected_rows.len()
                ));
            } else {
                summary.push_str(&format!(" · 已选 {} 行", self.selected_rows.len()));
            }
        }

        let mut root = v_flex()
            .size_full()
            .bg(bg)
            .child(toolbar::render(self, cx))
            .child(div().h(px(1.0)).bg(border));
        if let Some(notice) = self.memory_notice.clone() {
            let warn = cx.theme().warning;
            let mut warn_bg = warn;
            warn_bg.a = 0.14;
            root = root.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(5.0))
                    .bg(warn_bg)
                    .text_xs()
                    .text_color(warn)
                    .child(format!("⚠ {notice}")),
            );
        }
        if truncated {
            let warn = cx.theme().warning;
            let mut warn_bg = warn;
            warn_bg.a = 0.14;
            let message = if self.pagination.is_some() {
                format!(
                    "⚠ 当前页达到 256 MiB 硬上限，仅加载 {total_docs} 条；可从实际断点继续翻页，统计、排序、过滤与导出均只基于当前页。"
                )
            } else {
                format!(
                    "⚠ 结果较大，仅加载前 {total_docs} 条；统计、排序、过滤与导出均基于这部分数据。请用 filter / limit 精确查询"
                )
            };
            root = root.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(5.0))
                    .bg(warn_bg)
                    .text_xs()
                    .text_color(warn)
                    .child(message),
            );
        }
        if discovered_cols > total_cols {
            let warn = cx.theme().warning;
            let mut warn_bg = warn;
            warn_bg.a = 0.14;
            root = root.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(5.0))
                    .bg(warn_bg)
                    .text_xs()
                    .text_color(warn)
                    .child(format!(
                        "⚠ 字段较多，表格仅展示前 {total_cols} 列；完整文档详情仍保留，表格筛选与 CSV 导出基于已展示列。"
                    )),
            );
        }
        if self.is_drilled() {
            root = root.child(self.render_breadcrumb(cx));
        }
        root.child(div().flex_1().min_h_0().child(table::render(
            self,
            table_arc,
            col_indices,
            row_indices,
            true,
            None,
            cx,
        )))
        .child(render_status_bar(
            summary,
            border,
            muted,
            bg,
            self.pagination,
            cx.entity(),
        ))
        .into_any_element()
    }
}

fn render_status_bar(
    summary: String,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    bg: gpui::Hsla,
    pagination: Option<MongoResultPagination>,
    panel: Entity<ResultPanel>,
) -> impl IntoElement {
    h_flex()
        .id("mongo-status-bar")
        .w_full()
        .flex_none()
        .items_center()
        .gap_2()
        .px(px(12.0))
        .py(px(4.0))
        .border_t_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .text_color(muted)
        .child(SharedString::from(summary))
        .child(div().flex_1())
        .when_some(pagination, |this, pagination| {
            let previous_page = pagination.page.saturating_sub(1);
            let next_page = pagination.page.saturating_add(1);
            let panel_for_previous = panel.clone();
            let panel_for_next = panel.clone();
            this.child(
                ramag_ui::clickable_button("mongo-result-page-previous")
                    .ghost()
                    .small()
                    .label("上页")
                    .disabled(pagination.page == 0)
                    .on_click(move |_, _, app| {
                        panel_for_previous.update(app, |_, cx| {
                            cx.emit(ResultEvent::PageRequested(previous_page));
                        });
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .child(format!("第 {} 页", pagination.page + 1)),
            )
            .child(
                ramag_ui::clickable_button("mongo-result-page-next")
                    .ghost()
                    .small()
                    .label("下页")
                    .tooltip("未指定 sort 时分页顺序不固定")
                    .disabled(!pagination.has_more)
                    .on_click(move |_, _, app| {
                        panel_for_next.update(app, |_, cx| {
                            cx.emit(ResultEvent::PageRequested(next_page));
                        });
                    }),
            )
        })
}

fn flatten_hint(
    path: &str,
    n: usize,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    bg: gpui::Hsla,
) -> impl IntoElement {
    div()
        .id("mongo-flatten-hint")
        .w_full()
        .flex_none()
        .px(px(12.0))
        .py(px(5.0))
        .border_b_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .text_color(muted)
        .child(SharedString::from(format!(
            "已钻取「{path}」· {n} 条（清空上方过滤列恢复）"
        )))
}

fn empty_hint(text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    div()
        .id("mongo-result-hint")
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(12.0))
        .py(px(10.0))
        .text_xs()
        .text_color(color)
        .child(text.into())
}

fn error_hint(text: String, color: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    div()
        .id("mongo-result-error")
        .flex_1()
        .px(px(12.0))
        .py(px(10.0))
        .text_xs()
        .text_color(color)
        .child(SharedString::from(text))
}
