use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui::{Modifiers, TestAppContext, px, size};
use ramag_app::ConnectionService;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, QueryRecord, QueryResult, Row, Value,
};
use ramag_domain::error::Result;
use ramag_domain::traits::Storage;

use super::{QueryResultTarget, QueryTab, ResultState};
use crate::sql_completion::SchemaCache;

struct NoopStorage;

#[async_trait::async_trait]
impl Storage for NoopStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(Vec::new())
    }

    async fn get_connection(&self, _id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }

    async fn save_connection(&self, _config: &ConnectionConfig) -> Result<()> {
        Ok(())
    }

    async fn delete_connection(&self, _id: &ConnectionId) -> Result<()> {
        Ok(())
    }

    async fn append_history(&self, _record: &QueryRecord) -> Result<()> {
        Ok(())
    }

    async fn list_history(
        &self,
        _connection_id: Option<&ConnectionId>,
        _limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        Ok(Vec::new())
    }

    async fn delete_history(&self, _id: &ramag_domain::entities::QueryRecordId) -> Result<()> {
        Ok(())
    }

    async fn clear_history(&self, _connection_id: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }

    async fn get_preference(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set_preference(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
}

/// Context invalidation must release the UI wait state without deleting a completed result.
#[gpui::test]
fn invalidating_query_context_discards_running_state_but_keeps_ready_result(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let service = Arc::new(ConnectionService::new(
        HashMap::new(),
        Arc::new(NoopStorage),
    ));
    let schema_cache = SchemaCache::new_shared();
    let (tab, cx) = cx.add_window_view(|window, cx| {
        QueryTab::new(
            service,
            "查询 1",
            None,
            schema_cache,
            ramag_ui::ResultMemoryBudget::default(),
            window,
            cx,
        )
    });

    tab.update(cx, |tab, cx| {
        let result = QueryResult {
            columns: vec!["id".into()],
            column_types: vec!["BIGINT".into()],
            rows: vec![Row {
                values: vec![Value::Int(1)],
            }],
            affected_rows: 0,
            elapsed_ms: 1,
            warnings: Vec::new(),
            truncated: false,
        };
        tab.result.update(cx, |panel, cx| {
            panel.set_state(ResultState::Ok(Arc::new(result)), cx);
        });
        tab.plan_result.update(cx, |panel, cx| {
            panel.set_state(ResultState::Error("执行计划失败".into()), cx);
        });
        tab.set_plan_visible(true, cx);
        assert!(tab.show_plan);
        assert!(matches!(
            tab.active_result().read(cx).state(),
            ResultState::Error(_)
        ));
        tab.set_plan_visible(false, cx);
        assert!(!tab.show_plan);
    });

    tab.update(cx, |tab, cx| {
        tab.running = true;
        tab.run_seq = 7;
        tab.count_seq = 3;
        tab.running_target = Some(QueryResultTarget::Data);
        tab.query_start = Some(Instant::now());
        tab.invalidate_query_context(cx);

        assert!(!tab.running);
        assert!(tab.current_task.is_none());
        assert!(tab.running_target.is_none());
        assert!(tab.query_start.is_none());
        assert!(tab.run_seq != 7 && tab.count_seq != 3);
        assert!(matches!(tab.result.read(cx).state(), ResultState::Ok(_)));

        tab.running = true;
        tab.result.update(cx, |panel, cx| {
            panel.set_state(ResultState::Running, cx);
        });
        tab.invalidate_query_context(cx);
        assert!(matches!(tab.result.read(cx).state(), ResultState::Empty));
    });
}

/// The plan tab must render independently even when the data result already exists.
#[gpui::test]
fn plan_result_tabs_render_without_replacing_data_panel(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let service = Arc::new(ConnectionService::new(
        HashMap::new(),
        Arc::new(NoopStorage),
    ));
    let schema_cache = SchemaCache::new_shared();
    let (tab, cx) = cx.add_window_view(|window, cx| {
        QueryTab::new(
            service,
            "查询 1",
            None,
            schema_cache,
            ramag_ui::ResultMemoryBudget::default(),
            window,
            cx,
        )
    });

    tab.update(cx, |tab, cx| {
        tab.result.update(cx, |panel, cx| {
            panel.set_state(ResultState::Error("数据结果".into()), cx)
        });
        tab.plan_result.update(cx, |panel, cx| {
            panel.set_state(ResultState::Error("执行计划".into()), cx);
        });
        tab.set_plan_visible(true, cx);
    });
    tab.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("sql-result-view-tabs").is_some(),
        "SQL query tab should expose data/plan result tabs"
    );
    tab.read_with(cx, |tab, cx| {
        assert!(tab.show_plan);
        assert!(matches!(tab.result.read(cx).state(), ResultState::Error(_)));
        assert!(matches!(
            tab.active_result().read(cx).state(),
            ResultState::Error(_)
        ));
    });
}

/// Narrow result toolbars must keep both filter inputs and the run action reachable.
#[gpui::test]
fn result_toolbar_keeps_filters_and_run_action_inside_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let service = Arc::new(ConnectionService::new(
        HashMap::new(),
        Arc::new(NoopStorage),
    ));
    let schema_cache = SchemaCache::new_shared();
    let (tab, cx) = cx.add_window_view(|window, cx| {
        QueryTab::new(
            service,
            "查询 1",
            None,
            schema_cache,
            ramag_ui::ResultMemoryBudget::default(),
            window,
            cx,
        )
    });

    for width in [360.0, 1024.0, 1440.0] {
        cx.simulate_resize(size(px(width), px(480.0)));
        tab.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();

        let toolbar = cx
            .debug_bounds("sql-result-toolbar")
            .expect("SQL 结果工具栏应渲染");
        let filters = cx
            .debug_bounds("sql-result-filter-group")
            .expect("SQL 结果筛选区应渲染");
        let run = cx
            .debug_bounds("sql-run-query")
            .expect("SQL 运行按钮应渲染");

        assert!(filters.size.width > px(0.0), "筛选区不能被压缩为零宽");
        assert!(filters.right() <= toolbar.right(), "筛选区不能越出工具栏");
        assert!(run.right() <= toolbar.right(), "运行按钮不能越出工具栏");
    }
}

/// 查询失败时，重试按钮和长错误文本在窄窗口内保持可见，并重新走当前编辑器内容。
#[gpui::test]
fn sql_failure_retry_stays_inside_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let service = Arc::new(ConnectionService::new(
        HashMap::new(),
        Arc::new(NoopStorage),
    ));
    let schema_cache = SchemaCache::new_shared();
    let (tab, cx) = cx.add_window_view(|window, cx| {
        QueryTab::new(
            service,
            "查询失败重试",
            None,
            schema_cache,
            ramag_ui::ResultMemoryBudget::default(),
            window,
            cx,
        )
    });

    tab.update_in(cx, |tab, window, cx| {
        tab.editor.update(cx, |editor, cx| {
            editor.set_value("SELECT 1".to_string(), window, cx);
        });
        tab.result.update(cx, |result, cx| {
            result.set_state(
                ResultState::Error(
                    "数据库连接暂时不可用，请检查网络、凭据和当前连接上下文后重试".into(),
                ),
                cx,
            );
        });
        cx.notify();
    });

    for width in [360.0, 1024.0, 1440.0] {
        cx.simulate_resize(size(px(width), px(480.0)));
        tab.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();

        let error = cx
            .debug_bounds("sql-result-error")
            .expect("SQL 错误区域应渲染");
        let retry = cx
            .debug_bounds("sql-result-retry")
            .expect("SQL 错误区域应提供重试按钮");
        assert!(error.right() <= px(width), "错误区域不能越出窗口");
        assert!(retry.right() <= error.right(), "重试按钮不能越出错误区域");
        assert!(
            retry.bottom() <= error.bottom(),
            "重试按钮不能被错误区域裁掉"
        );
    }

    let retry = cx
        .debug_bounds("sql-result-retry")
        .expect("SQL 错误区域应提供重试按钮");
    cx.simulate_click(retry.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(matches!(
        tab.read_with(cx, |tab, cx| tab.result.read(cx).state().clone()),
        ResultState::Error(message) if message == "尚未选择连接"
    ));
}

/// A failed transaction operation must have a distinct status from normal auto-commit mode.
#[gpui::test]
fn transaction_failure_status_is_explicit(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let service = Arc::new(ConnectionService::new(
        HashMap::new(),
        Arc::new(NoopStorage),
    ));
    let schema_cache = SchemaCache::new_shared();
    let (tab, cx) = cx.add_window_view(|window, cx| {
        QueryTab::new(
            service,
            "查询 1",
            None,
            schema_cache,
            ramag_ui::ResultMemoryBudget::default(),
            window,
            cx,
        )
    });

    tab.update(cx, |tab, _cx| {
        assert_eq!(tab.transaction_label(), "自动提交");
        tab.transaction_error = Some("事务语句失败".into());
        assert_eq!(tab.transaction_label(), "事务异常");
        tab.transaction_busy = true;
        assert_eq!(tab.transaction_label(), "事务处理中");
    });
}

/// A plan request owns only the plan panel; normal requests own the data panel.
#[test]
fn query_result_target_matches_request_kind() {
    assert_eq!(
        QueryResultTarget::from_plan_request(None),
        QueryResultTarget::Data
    );
    assert_eq!(
        QueryResultTarget::from_plan_request(Some(1)),
        QueryResultTarget::Plan
    );
}
