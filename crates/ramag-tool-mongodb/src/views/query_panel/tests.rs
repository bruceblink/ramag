use std::sync::Arc;

use gpui::{AppContext as _, TestAppContext, px, size};
use ramag_app::MongoService;
use ramag_domain::entities::{ConnectionConfig, ConnectionId, QueryRecord};
use ramag_domain::error::Result;
use ramag_domain::traits::Storage;

use super::MongoQueryPanel;

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

/// 三种窗口下 MongoDB 查询工具栏都要保留标签区和操作区。
#[gpui::test]
fn query_toolbar_keeps_actions_visible_in_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        Arc::new(NoopStorage),
    ));
    let mut panel_entity = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| {
            MongoQueryPanel::new(service, ramag_ui::ResultMemoryBudget::default(), window, cx)
        });
        panel_entity = Some(panel.clone());
        gpui_component::Root::new(panel, window, cx)
    });
    let panel = panel_entity.expect("MongoDB 查询面板应创建");

    cx.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_connection(
                Some(ConnectionConfig::new_mongodb(
                    "测试连接",
                    "127.0.0.1",
                    27017,
                )),
                window,
                cx,
            );
            for _ in 0..6 {
                assert!(panel.add_tab(window, cx));
            }
            assert!(panel.toggle_editor(window, cx));
        });
    });

    for width in [360.0, 1024.0, 1440.0] {
        cx.simulate_resize(size(px(width), px(480.0)));
        panel.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();

        let toolbar = cx
            .debug_bounds("mongo-editor-toolbar")
            .expect("MongoDB 查询工具栏应渲染");
        let actions = cx
            .debug_bounds("mongo-editor-actions")
            .expect("MongoDB 查询操作区应渲染");

        assert!(toolbar.right() <= px(width), "工具栏不能越出窗口");
        assert!(actions.size.width > px(0.0), "操作区不能被压缩为零宽");
        assert!(actions.right() <= toolbar.right(), "操作区不能越出工具栏");
        assert!(
            actions.bottom() <= toolbar.bottom(),
            "操作区不能被工具栏裁掉"
        );
    }
}
