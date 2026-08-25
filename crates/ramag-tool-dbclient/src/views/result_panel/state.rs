use super::*;

impl ResultPanel {
    pub fn set_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
        let state = self.account_result_memory(state, cx);
        if matches!(&state, ResultState::Released(_)) {
            self.clear_released_result_context();
        }
        let has_client_warning = matches!(
            &state,
            ResultState::Ok(qr) if qr.warnings.iter().any(|warning| warning.level == "Client")
        );
        match &state {
            ResultState::Ok(qr) => {
                *self.column_completion_source.write() = qr.columns.clone();
            }
            _ => {
                self.column_completion_source.write().clear();
            }
        }
        self.state = state;
        self.pagination = None;
        self.mark_result_changed();
        self.clear_cell_edit_state();
        self.selected_cell = None;
        self.clear_selected_rows();
        self.sort_by = None;
        self.col_width_overrides.clear();
        self.pending_insert = None;
        // 客户端资源警告直接展开，避免用户把已截断结果误认为完整结果。
        self.warnings_expanded = has_client_warning;
        self.row_identity = None;
        self.uniform_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.h_scroll.set_offset(Point::new(px(0.0), px(0.0)));
        self.result_scroll_gesture.reset();
        cx.notify();
    }

    /// 恢复状态快照，不清理选择、排序与滚动位置。
    pub fn restore_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
        let state = self.account_result_memory(state, cx);
        if matches!(&state, ResultState::Released(_)) {
            self.clear_released_result_context();
        }
        if let ResultState::Ok(qr) = &state {
            *self.column_completion_source.write() = qr.columns.clone();
        }
        self.state = state;
        self.mark_result_changed();
        self.clear_cell_edit_state();
        cx.notify();
    }

    pub(crate) fn account_result_memory(
        &self,
        mut state: ResultState,
        cx: &mut Context<Self>,
    ) -> ResultState {
        let bytes = match &state {
            ResultState::Ok(result) => {
                usize::try_from(result.retained_bytes()).unwrap_or(usize::MAX)
            }
            _ => 0,
        };
        let Some(lease) = &self.result_memory else {
            return state;
        };
        let outcome = lease.update_bytes(bytes, cx);
        if outcome.current_evicted {
            return ResultState::Released(
                "结果超过全部标签 512 MiB 硬上限，已释放结果数据；查询文本仍保留，可收窄后重新运行"
                    .into(),
            );
        }
        if outcome.warning
            && let ResultState::Ok(result) = &mut state
        {
            let result = Arc::make_mut(result);
            if !result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("全部查询标签结果"))
            {
                result.warnings.push(global_memory_warning(outcome));
            }
        }
        state
    }

    /// 释放旧结果，保留编辑器中的查询文本。
    pub fn evict_result_for_budget(&mut self, cx: &mut Context<Self>) {
        self.state = ResultState::Released(
            "旧结果已按 LRU 释放，以保持全部标签结果不超过 512 MiB；查询文本仍保留".into(),
        );
        self.clear_released_result_context();
        self.column_completion_source.write().clear();
        self.pagination = None;
        self.mark_result_changed();
        self.clear_cell_edit_state();
        self.selected_cell = None;
        self.clear_selected_rows();
        self.sort_by = None;
        self.col_width_overrides.clear();
        self.pending_insert = None;
        self.row_identity = None;
        cx.notify();
    }

    pub(crate) fn clear_released_result_context(&mut self) {
        self.source_sql = None;
        self.pinned_target = None;
        self.clear_cell_edit_state();
        self.row_identity = None;
    }

    /// 标记结果变化并丢弃派生缓存。
    pub(crate) fn mark_result_changed(&mut self) {
        self.result_revision = self.result_revision.wrapping_add(1);
        self.invalidate_display_view();
    }

    /// 取消旧派生任务并释放索引。
    pub(crate) fn invalidate_display_view(&mut self) {
        self.cancel_display_view_build();
        self.display_view_request_seq = self.display_view_request_seq.wrapping_add(1);
        self.display_view_cache = None;
        self.display_view_build_key = None;
        self.display_view_building = false;
        self.display_view_error = None;
    }

    pub(crate) fn cancel_display_view_build(&mut self) {
        if let Some(cancelled) = self.display_view_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn selected_rows(&self) -> &BTreeSet<usize> {
        &self.selected_rows
    }

    pub(crate) fn toggle_row_selected(&mut self, ri: usize, cx: &mut Context<Self>) {
        if !self.selected_rows.remove(&ri) {
            self.selected_rows.insert(ri);
        }
        self.mark_selection_changed();
        cx.notify();
    }

    pub(crate) fn toggle_visible_rows(&mut self, visible: &[usize], cx: &mut Context<Self>) {
        toggle_visible_selection(&mut self.selected_rows, visible);
        self.mark_selection_changed();
        cx.notify();
    }

    pub(crate) fn clear_selected_rows(&mut self) {
        self.selected_rows.clear();
        self.mark_selection_changed();
    }

    pub(crate) fn visible_selection_summary(&mut self, visible: &Arc<Vec<usize>>) -> (usize, bool) {
        if let Some(cache) = &self.visible_selection_cache
            && cache.selection_revision == self.selection_revision
            && Arc::ptr_eq(&cache.rows, visible)
        {
            return (
                cache.visible_selected,
                !visible.is_empty() && cache.visible_selected == visible.len(),
            );
        }
        let visible_selected = visible_selection_count(&self.selected_rows, visible);
        self.visible_selection_cache = Some(VisibleSelectionCache {
            rows: visible.clone(),
            selection_revision: self.selection_revision,
            visible_selected,
        });
        (
            visible_selected,
            !visible.is_empty() && visible_selected == visible.len(),
        )
    }

    pub(crate) fn mark_selection_changed(&mut self) {
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.visible_selection_cache = None;
    }

    pub(crate) fn set_col_width_override(&mut self, col_ix: usize, width: gpui::Pixels) {
        let n_cols = match &self.state {
            ResultState::Ok(r) => r.columns.len(),
            _ => return,
        };
        if self.col_width_overrides.len() != n_cols {
            self.col_width_overrides.resize(n_cols, None);
        }
        if col_ix < self.col_width_overrides.len() {
            self.col_width_overrides[col_ix] = Some(width);
        }
    }

    pub(crate) fn col_width_override(&self, col_ix: usize) -> Option<gpui::Pixels> {
        self.col_width_overrides.get(col_ix).copied().flatten()
    }

    pub(crate) fn toggle_sort(&mut self, col_idx: usize, cx: &mut Context<Self>) {
        let previous = self.sort_by;
        let current = match previous {
            Some((ci, SortDir::Asc)) if ci == col_idx => Some((col_idx, SortDir::Desc)),
            Some((ci, SortDir::Desc)) if ci == col_idx => None,
            _ => Some((col_idx, SortDir::Asc)),
        };
        self.sort_by = current;
        self.clear_cell_edit_state();
        self.selected_cell = None;
        self.invalidate_display_view();
        cx.emit(super::ResultPanelEvent::SortChanged { previous, current });
        cx.notify();
    }

    pub(crate) fn restore_sort(
        &mut self,
        sort_by: Option<(usize, SortDir)>,
        cx: &mut Context<Self>,
    ) {
        if self.sort_by == sort_by {
            return;
        }
        self.sort_by = sort_by;
        self.invalidate_display_view();
        cx.notify();
    }

    pub(crate) fn sort_by(&self) -> Option<(usize, SortDir)> {
        self.sort_by
    }

    pub(crate) fn pagination(&self) -> Option<ResultPagination> {
        self.pagination
    }

    pub(crate) fn set_pagination(
        &mut self,
        pagination: Option<ResultPagination>,
        cx: &mut Context<Self>,
    ) {
        if self.pagination == pagination {
            return;
        }
        self.pagination = pagination;
        cx.notify();
    }

    /// 仍有分页结果时回填精确总数。
    pub(crate) fn set_pagination_total(&mut self, total: TotalRows, cx: &mut Context<Self>) {
        let Some(pagination) = self.pagination.as_mut() else {
            return;
        };
        if pagination.total == total {
            return;
        }
        pagination.total = total;
        cx.notify();
    }

    /// Shows a bounded page-size error in the result toolbar without starting a database request.
    pub(crate) fn notify_page_size_error(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.pending_notification = Some(Notification::error(message.into()).autohide(true));
        cx.notify();
    }

    pub(crate) fn selected_cell(&self) -> Option<(usize, usize)> {
        self.selected_cell
    }

    pub(crate) fn set_selected_cell(&mut self, cell: Option<(usize, usize)>) {
        self.selected_cell = cell;
    }

    /// 切换结果数据源时列结构会变化；内容搜索作为用户条件跨表保留。
    pub fn clear_column_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.column_filter_input
            .update(cx, |s, cx| s.set_value("", window, cx));
    }

    pub(crate) fn column_filter_text(&self, cx: &gpui::App) -> String {
        self.column_filter_input.read(cx).value().trim().to_string()
    }

    pub(crate) fn row_filter_text(&self, cx: &gpui::App) -> String {
        self.row_filter_input.read(cx).value().trim().to_string()
    }

    pub fn column_filter_entity(&self) -> &Entity<InputState> {
        &self.column_filter_input
    }
    pub fn row_filter_entity(&self) -> &Entity<InputState> {
        &self.row_filter_input
    }

    pub fn state(&self) -> &ResultState {
        &self.state
    }
}
