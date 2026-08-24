use std::collections::HashMap;
use std::sync::Arc;

use gpui::TestAppContext;
use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, ConnectionId, QueryRecord};
use ramag_domain::error::Result;
use ramag_domain::traits::Storage;

use super::{
    ClosedQueryDraft, MAX_CLOSED_QUERY_DRAFTS, QueryPanel, active_index_after_close,
    push_closed_draft,
};
use crate::sql_completion::SchemaCache;
use crate::views::result_panel::ResultState;

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

#[test]
fn closing_tab_left_of_active_preserves_the_same_logical_tab() {
    assert_eq!(active_index_after_close(1, 0, 2), 0);
}

#[test]
fn closing_active_last_tab_activates_new_last_tab() {
    assert_eq!(active_index_after_close(2, 2, 2), 1);
}

#[test]
fn closing_tab_right_of_active_keeps_index() {
    assert_eq!(active_index_after_close(0, 2, 2), 0);
}

#[test]
fn closed_draft_stack_is_bounded_and_reopens_newest_first() {
    let mut stack = Vec::new();
    for index in 0..=MAX_CLOSED_QUERY_DRAFTS {
        push_closed_draft(
            &mut stack,
            ClosedQueryDraft {
                title: format!("查询 {index}"),
                text: format!("SELECT {index}").into(),
                context: None,
            },
        );
    }

    assert_eq!(stack.len(), MAX_CLOSED_QUERY_DRAFTS);
    assert_eq!(
        stack.first().map(|draft| draft.title.as_str()),
        Some("查询 1")
    );
    assert_eq!(stack.pop().map(|draft| draft.title), Some("查询 10".into()));
}

/// Session disposal must invalidate every SQL tab independently without clearing terminal results.
#[gpui::test]
fn cancel_pending_queries_invalidates_all_tabs_without_clearing_terminal_results(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let service = Arc::new(ConnectionService::new(
        HashMap::new(),
        Arc::new(NoopStorage),
    ));
    let schema_cache = SchemaCache::new_shared();
    let (panel, cx) = cx.add_window_view(|window, cx| {
        QueryPanel::new(
            service,
            schema_cache,
            ramag_ui::ResultMemoryBudget::default(),
            window,
            cx,
        )
    });

    cx.update(|window, app| {
        panel.update(app, |panel, cx| {
            assert!(panel.add_tab(window, cx));
            for (index, tab) in panel.tabs.iter().enumerate() {
                tab.update(cx, |tab, cx| {
                    tab.result.update(cx, |result, cx| {
                        result.set_state(ResultState::Error(format!("ready-{index}")), cx);
                    });
                    tab.running = true;
                    tab.run_seq = index as u64 + 1;
                });
            }
            panel.cancel_pending_queries(cx);
        });
    });
    let states = cx.update(|_, app| {
        panel
            .read(app)
            .tabs
            .iter()
            .map(|tab| {
                let tab = tab.read(app);
                (
                    tab.running,
                    tab.run_seq,
                    matches!(tab.result.read(app).state(), ResultState::Error(_)),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(states, vec![(false, 2, true), (false, 3, true)]);
}
