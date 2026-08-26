//! 查询结果基准快照、范围说明和内存登记。

use std::sync::Arc;

use gpui::{AppContext as _, Context, ParentElement as _, Window, px};
use gpui_component::{WindowExt as _, notification::Notification};
use ramag_domain::entities::QueryResult;

use crate::views::result_diff::{ResultScopeKey, ResultSnapshot};
use crate::views::result_diff_dialog::ResultDiffDialog;

use super::{ResultPanel, ResultState, TotalRows};

impl ResultPanel {
    pub fn set_source_sql(&mut self, sql: Option<String>) {
        self.source_sql = sql;
    }

    pub(crate) fn set_source_schema(&mut self, schema: Option<String>) {
        self.source_schema = schema;
    }

    pub(crate) fn source_sql(&self) -> Option<String> {
        self.source_sql.clone()
    }

    pub(crate) fn source_schema(&self) -> Option<String> {
        self.source_schema.clone()
    }

    pub(crate) fn result_revision(&self) -> u64 {
        self.result_revision
    }

    pub(crate) fn can_capture_comparison_baseline(&self) -> bool {
        matches!(&self.state, ResultState::Ok(_))
    }

    pub(crate) fn has_comparison_baseline(&self) -> bool {
        self.comparison_baseline.is_some()
    }

    pub(crate) fn capture_comparison_baseline(&mut self, cx: &mut Context<Self>) -> bool {
        let ResultState::Ok(result) = &self.state else {
            return false;
        };
        self.comparison_baseline = Some(self.build_result_snapshot(result.clone(), "基准"));
        self.refresh_comparison_memory(cx);
        cx.notify();
        true
    }

    pub(crate) fn clear_comparison_baseline(&mut self, cx: &mut Context<Self>) {
        if self.comparison_baseline.take().is_some() {
            self.refresh_comparison_memory(cx);
            cx.notify();
        }
    }

    pub(crate) fn comparison_inputs(&self) -> Option<(ResultSnapshot, ResultSnapshot)> {
        let baseline = self.comparison_baseline.clone()?;
        let ResultState::Ok(current) = &self.state else {
            return None;
        };
        Some((
            baseline,
            self.build_result_snapshot(current.clone(), "当前"),
        ))
    }

    pub(crate) fn current_comparison_snapshot(&self, role: &str) -> Option<ResultSnapshot> {
        let ResultState::Ok(result) = &self.state else {
            return None;
        };
        Some(self.build_result_snapshot(result.clone(), role))
    }

    pub(crate) fn open_comparison_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((source, target)) = self.comparison_inputs() else {
            self.pending_notification = Some(
                Notification::warning("请先保存基准，并确保当前结果已执行完成").autohide(true),
            );
            cx.notify();
            return;
        };
        let panel = cx.new(|cx| ResultDiffDialog::new(source, target, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let panel_for_content = panel.clone();
            dialog
                .title("查询结果差异")
                .width(px(1_120.0))
                .margin_top(px(55.0))
                .content(move |content, _, _| content.child(panel_for_content.clone()))
        });
    }

    /// 基准与当前结果可能共享同一个 Arc；只为实际保留的结果计算一次预算。
    pub(crate) fn retained_bytes_with_comparison(&self, state: &ResultState) -> usize {
        let current = match state {
            ResultState::Ok(result) => Some(result),
            _ => None,
        };
        let current_bytes = current
            .map(|result| usize::try_from(result.retained_bytes()).unwrap_or(usize::MAX))
            .unwrap_or(0);
        let Some(baseline) = self.comparison_baseline.as_ref() else {
            return current_bytes;
        };
        if current.is_some_and(|result| Arc::ptr_eq(result, &baseline.result)) {
            return current_bytes;
        }
        current_bytes
            .saturating_add(usize::try_from(baseline.result.retained_bytes()).unwrap_or(usize::MAX))
    }

    fn refresh_comparison_memory(&mut self, cx: &mut Context<Self>) {
        let bytes = self.retained_bytes_with_comparison(&self.state);
        let outcome = self
            .result_memory
            .as_ref()
            .map(|lease| lease.update_bytes(bytes, cx));
        if outcome.is_some_and(|outcome| outcome.current_evicted) {
            self.comparison_baseline = None;
            self.set_state(
                ResultState::Released(
                "结果超过全部标签 512 MiB 硬上限，已释放结果数据；查询文本仍保留，可收窄后重新运行"
                    .into(),
                ),
                cx,
            );
        }
    }

    fn build_result_snapshot(&self, result: Arc<QueryResult>, role: &str) -> ResultSnapshot {
        let (scope, scope_key) = self.result_scope(&result);
        ResultSnapshot::from_query(
            result,
            role,
            self.connection.as_ref(),
            self.source_sql.as_deref(),
            scope,
            scope_key,
            self.pinned_target.clone(),
            self.row_identity
                .as_ref()
                .map(|identity| identity.columns.clone()),
            self.source_schema.clone(),
        )
    }

    fn result_scope(&self, result: &QueryResult) -> (String, ResultScopeKey) {
        match self.pagination {
            Some(pagination) => {
                let total = match pagination.total {
                    TotalRows::Counting => " / 总数统计中".to_string(),
                    TotalRows::Known(total) => format!(" / 共 {total} 行"),
                    TotalRows::Unavailable => " / 总数未知".to_string(),
                };
                (
                    format!(
                        "第 {} 页 · 已加载 {} 行{}",
                        pagination.page.saturating_add(1),
                        result.rows.len(),
                        total
                    ),
                    ResultScopeKey {
                        page: Some(pagination.page),
                        page_size: Some(pagination.page_size),
                        truncated: result.truncated,
                    },
                )
            }
            None => (
                format!(
                    "已加载 {} 行{}",
                    result.rows.len(),
                    if result.truncated {
                        " · 结果已截断"
                    } else {
                        ""
                    }
                ),
                ResultScopeKey {
                    truncated: result.truncated,
                    ..ResultScopeKey::default()
                },
            ),
        }
    }
}
