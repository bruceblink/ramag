//! 查询历史后台筛选：输入防抖、任务取消与旧结果隔离。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::Context;
use ramag_domain::entities::QueryRecord;

use super::MongoHistoryList;

/// 连续输入时只处理最后一次搜索，避免为中间关键字浪费 CPU。
const FILTER_DEBOUNCE: Duration = Duration::from_millis(160);

impl MongoHistoryList {
    /// 在共享 worker 中筛选大历史正文；generation 与独立 token 共同阻止旧结果回写。
    pub(super) fn schedule_filter(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.cancel_filter();
        self.filter_generation = self.filter_generation.wrapping_add(1);
        let generation = self.filter_generation;
        self.filter_query = query.clone();
        self.filter_error = None;

        if query.is_empty() {
            self.filtered_indices = Arc::new((0..self.records.len()).collect());
            self.filtering = false;
            cx.notify();
            return;
        }

        self.filtered_indices = Arc::new(Vec::new());
        self.filtering = true;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.filter_cancel = Some(cancelled.clone());
        let records = self.records.clone();
        cx.notify();

        cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(FILTER_DEBOUNCE).await;
            }
            let current = this
                .update(cx, |this, _| {
                    this.filter_generation == generation
                        && this.filter_query == query
                        && this
                            .filter_cancel
                            .as_ref()
                            .is_some_and(|token| Arc::ptr_eq(token, &cancelled))
                })
                .unwrap_or(false);
            if !current {
                return;
            }

            let worker_cancelled = cancelled.clone();
            let worker_query = query.clone();
            let result = ramag_app::run_blocking(move || {
                Ok(filter_history_indices(
                    records.as_slice(),
                    &worker_query,
                    &worker_cancelled,
                ))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.filter_generation != generation
                    || this.filter_query != query
                    || !this
                        .filter_cancel
                        .as_ref()
                        .is_some_and(|token| Arc::ptr_eq(token, &cancelled))
                {
                    return;
                }
                this.filter_cancel = None;
                this.filtering = false;
                match result {
                    Ok(Some(indices)) => this.filtered_indices = Arc::new(indices),
                    Ok(None) => {}
                    Err(error) => {
                        this.filter_error = Some(format!("搜索历史失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_filter(&mut self) {
        if let Some(cancelled) = self.filter_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for MongoHistoryList {
    fn drop(&mut self) {
        self.cancel_filter();
    }
}

fn filter_history_indices(
    records: &[Arc<QueryRecord>],
    query: &str,
    cancelled: &AtomicBool,
) -> Option<Vec<usize>> {
    let mut indices = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        if record.matches_query_lower(query) {
            indices.push(index);
        }
    }
    (!cancelled.load(Ordering::Relaxed)).then_some(indices)
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::ConnectionId;

    use super::*;

    #[test]
    fn history_filter_returns_indices_and_honors_cancellation() {
        let connection_id = ConnectionId::new();
        let records = vec![
            Arc::new(QueryRecord::new_success(
                connection_id.clone(),
                "local",
                r#"{"find":"users"}"#,
                1,
                1,
            )),
            Arc::new(QueryRecord::new_failed(
                connection_id,
                "local",
                r#"{"delete":"sessions"}"#,
                "permission denied",
            )),
        ];
        let cancelled = AtomicBool::new(false);

        assert_eq!(
            filter_history_indices(&records, "permission", &cancelled),
            Some(vec![1])
        );
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(filter_history_indices(&records, "find", &cancelled), None);
    }
}
