//! MongoDB 结果行视图的筛选、排序与缓存。

use super::*;

impl ResultPanel {
    pub(crate) fn parse_column_filter(&self, cx: &gpui::App) -> ParsedFilter {
        let raw = self.column_filter.read(cx).value().to_string();
        let docs = self
            .drill_stack
            .last()
            .map(|level| level.documents.as_slice())
            .unwrap_or(&[]);
        classify_filter(&raw, docs)
    }

    pub(crate) fn schedule_table_rebuild(&mut self, cx: &mut Context<Self>) {
        let level = self.drill_stack.last();
        let docs = level
            .map(|l| l.documents.clone())
            .unwrap_or_else(|| Arc::new(Vec::new()));
        let ancestors = level.map(|l| l.ancestors.clone()).unwrap_or_default();
        self.cancel_table_build();
        self.table_build_seq = self.table_build_seq.wrapping_add(1);
        let request_seq = self.table_build_seq;
        self.table = None;
        let document_bytes = self.retained_document_bytes();
        let _ = self.account_result_bytes(document_bytes, cx);
        self.invalidate_row_view();
        self.table_building = !docs.is_empty();
        self.column_completion_source.write().clear();
        if docs.is_empty() {
            return;
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        self.table_build_cancel = Some(cancelled.clone());

        cx.spawn(async move |this, cx| {
            let worker_cancelled = cancelled.clone();
            let built = ramag_app::run_blocking(move || {
                let Some(mut ft) = flatten::build_flat_table_with_cancellable(
                    docs.as_slice(),
                    &BTreeSet::new(),
                    &worker_cancelled,
                ) else {
                    return Ok(None);
                };
                // 下钻层保留从根到父级的祖先列。
                if !ancestors.is_empty() {
                    let lead = ancestors
                        .into_iter()
                        .map(|(label, cell)| {
                            let kind = if cell.kind == "null" {
                                "text"
                            } else {
                                cell.kind
                            };
                            let path = drill::ancestor_id_column_name(&label);
                            (flatten::Column { path, kind }, cell)
                        })
                        .collect();
                    if !ft.prepend_constant_lead_cancellable(lead, &worker_cancelled) {
                        return Ok(None);
                    }
                }
                let Some(completions) = flatten::collect_paths_cancellable(
                    docs.as_slice(),
                    PATH_COMPLETION_DEPTH,
                    &worker_cancelled,
                ) else {
                    return Ok(None);
                };
                Ok(Some((Arc::new(ft), completions)))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.table_build_seq != request_seq
                    || !this
                        .table_build_cancel
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
                {
                    return;
                }
                this.table_build_cancel = None;
                this.table_building = false;
                match built {
                    Ok(Some((table, completions))) => {
                        let combined_bytes = this
                            .retained_document_bytes()
                            .saturating_add(table.retained_bytes());
                        let outcome = this.account_result_bytes(combined_bytes, cx);
                        if outcome.current_evicted {
                            this.release_result_payload();
                            this.error = Some(
                                "MongoDB 结果及表格视图超过全部标签 512 MiB 硬上限，已释放；请收窄查询"
                                    .into(),
                            );
                            cx.notify();
                            return;
                        }
                        if let Some(result) = &this.result
                            && let Some(notice) = memory_notice(result, combined_bytes, outcome)
                        {
                            // 表格占用更新不清除刚产生的 LRU 提示。
                            this.memory_notice = Some(notice);
                        }
                        this.table = Some(table);
                        *this.column_completion_source.write() = completions;
                        this.schedule_row_view(false, cx);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.invalidate_row_view();
                        this.error = Some(format!("构建 MongoDB 结果表格失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 原始结果之外，显式下钻层会持有嵌套值副本，也必须计入全局预算。
    pub(super) fn retained_document_bytes(&self) -> usize {
        self.result
            .as_ref()
            .map_or(0, |result| result.retained_bytes)
            .saturating_add(self.drill_stack.iter().fold(0usize, |total, level| {
                total.saturating_add(level.owned_bytes)
            }))
    }

    pub(crate) fn filtered_column_indices(&self, cx: &gpui::App) -> Option<Vec<usize>> {
        column_indices_for(self.table.as_ref()?, &self.parse_column_filter(cx).filters)
    }

    pub(crate) fn display_row_indices(&self, cx: &gpui::App) -> Option<(Arc<Vec<usize>>, bool)> {
        let filter = self.effective_row_filter(cx);
        let filtered = filter.is_active();
        let key = RowViewKey {
            generation: self.table_build_seq,
            filter,
            sort_by: self.sort_by.clone(),
        };
        self.row_view_cache
            .as_ref()
            .filter(|cache| cache.key == key)
            .map(|cache| (cache.indices.clone(), filtered))
    }

    pub(super) fn invalidate_row_view(&mut self) {
        self.cancel_row_view_build();
        self.row_view_request_seq = self.row_view_request_seq.wrapping_add(1);
        self.row_view_cache = None;
        self.row_view_building = false;
        self.row_view_error = None;
    }

    /// 去抖后在受限工作池扫描，旧条件回包按代际丢弃。
    pub(super) fn schedule_row_view(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let Some(table) = self.table.clone() else {
            self.invalidate_row_view();
            cx.notify();
            return;
        };
        let key = RowViewKey {
            generation: self.table_build_seq,
            filter: self.effective_row_filter(cx),
            sort_by: self.sort_by.clone(),
        };
        if self
            .row_view_cache
            .as_ref()
            .is_some_and(|cache| cache.key == key)
        {
            self.row_view_building = false;
            self.row_view_error = None;
            cx.notify();
            return;
        }

        self.cancel_row_view_build();
        self.row_view_request_seq = self.row_view_request_seq.wrapping_add(1);
        let request_seq = self.row_view_request_seq;
        self.row_view_cache = None;
        self.row_view_building = true;
        self.row_view_error = None;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.row_view_cancel = Some(cancelled.clone());
        cx.notify();

        cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(ROW_VIEW_DEBOUNCE).await;
            }
            let current = this
                .update(cx, |this, _| {
                    this.row_view_request_seq == request_seq
                        && this.table_build_seq == key.generation
                        && this
                            .row_view_cancel
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
                })
                .unwrap_or(false);
            if !current {
                return;
            }

            let worker_key = key.clone();
            let worker_cancelled = cancelled.clone();
            let built = ramag_app::run_blocking(move || {
                Ok(build_row_view_indices(
                    &table,
                    &worker_key,
                    &worker_cancelled,
                ))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.row_view_request_seq != request_seq
                    || this.table_build_seq != key.generation
                    || !this
                        .row_view_cancel
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
                {
                    return;
                }
                this.row_view_cancel = None;
                this.row_view_building = false;
                match built {
                    Ok(Some(indices)) => {
                        this.row_view_cache = Some(RowViewCache { key, indices });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.row_view_error = Some(format!("构建行视图失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn cancel_table_build(&mut self) {
        if let Some(cancelled) = self.table_build_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn cancel_row_view_build(&mut self) {
        if let Some(cancelled) = self.row_view_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}
