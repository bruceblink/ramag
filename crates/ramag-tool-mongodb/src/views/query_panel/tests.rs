use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, Window, div, px, size,
};
use gpui_component::WindowExt as _;
use ramag_app::MongoService;
use ramag_domain::entities::{ConnectionConfig, ConnectionId, QueryRecord};
use ramag_domain::error::Result;
use ramag_domain::traits::Storage;

use super::MongoQueryPanel;

#[derive(Default)]
struct NoopStorage {
    history: Vec<QueryRecord>,
}

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
        connection_id: Option<&ConnectionId>,
        limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        Ok(self
            .history
            .iter()
            .filter(|record| {
                connection_id
                    .map(|connection_id| record.connection_id == *connection_id)
                    .unwrap_or(true)
            })
            .take(limit)
            .cloned()
            .collect())
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

/// 测试宿主同时渲染 MongoDB 查询面板和 GPUI Component 的对话框浮层。
struct HistoryDialogTestHost {
    panel: Entity<MongoQueryPanel>,
}

impl Render for HistoryDialogTestHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
        div()
            .relative()
            .size_full()
            .child(self.panel.clone())
            .children(dialog_layer)
    }
}

/// 三种窗口下 MongoDB 查询工具栏都要保留标签区和操作区。
#[gpui::test]
fn query_toolbar_keeps_actions_visible_in_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        Arc::new(NoopStorage::default()),
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

/// 查询历史弹框应在三种窗口宽度内保留搜索、标题和列表区域。
#[gpui::test]
fn history_dialog_stays_inside_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let connection = ConnectionConfig::new_mongodb("历史测试连接", "127.0.0.1", 27017);
    let service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        Arc::new(NoopStorage {
            history: (0..=200)
                .map(|index| {
                    QueryRecord::new_failed(
                        connection.id.clone(),
                        "历史测试连接",
                        format!(
                            "db.orders.find({{ payload: {{ $regex: 'narrow-window-{index}' }} }})"
                        ),
                        "模拟失败：历史记录详情用于窄窗口布局验证",
                    )
                })
                .collect(),
        }),
    ));
    let mut panel_entity = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| {
            MongoQueryPanel::new(service, ramag_ui::ResultMemoryBudget::default(), window, cx)
        });
        panel_entity = Some(panel.clone());
        let host = cx.new(|_| HistoryDialogTestHost {
            panel: panel.clone(),
        });
        gpui_component::Root::new(host, window, cx)
    });
    let panel = panel_entity.expect("MongoDB 查询面板应创建");

    cx.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_connection(Some(connection), window, cx);
        });
    });
    cx.run_until_parked();

    for (width, height) in [(360.0, 620.0), (1024.0, 620.0), (1440.0, 620.0)] {
        cx.simulate_resize(size(px(width), px(height)));
        panel.update_in(cx, |panel, window, cx| {
            panel.open_history_dialog(window, cx);
        });
        cx.run_until_parked();

        let list = cx
            .debug_bounds("mongo-history-list")
            .expect("MongoDB 查询历史列表应渲染");
        let toolbar = cx
            .debug_bounds("mongo-history-toolbar")
            .expect("MongoDB 查询历史工具栏应渲染");
        let title = cx
            .debug_bounds("mongo-history-dialog-title")
            .expect("MongoDB 查询历史标题应渲染");
        let row = cx
            .debug_bounds("mongo-history-row-0")
            .expect("MongoDB 查询历史记录行应渲染");
        let actions = cx
            .debug_bounds("mongo-history-actions-0")
            .expect("MongoDB 查询历史操作组应渲染");
        let toolbar_children = [
            (
                "搜索区",
                cx.debug_bounds("mongo-history-search")
                    .expect("MongoDB 查询历史搜索区应渲染"),
            ),
            (
                "数量",
                cx.debug_bounds("mongo-history-count")
                    .expect("MongoDB 查询历史数量提示应渲染"),
            ),
            (
                "状态提示",
                cx.debug_bounds("mongo-history-warning")
                    .expect("MongoDB 查询历史状态提示应渲染"),
            ),
            (
                "清空按钮",
                cx.debug_bounds("mongo-history-clear-all")
                    .expect("MongoDB 查询历史清空按钮应渲染"),
            ),
        ];
        assert!(list.size.width > px(0.0), "MongoDB 查询历史列表不能为零宽");
        assert!(
            list.right() <= px(width),
            "MongoDB 查询历史列表不能越出窗口"
        );
        assert!(
            toolbar.right() <= list.right(),
            "MongoDB 查询历史工具栏不能越出列表"
        );
        assert!(
            title.right() <= px(width),
            "MongoDB 查询历史标题不能越出窗口"
        );
        assert!(
            row.right() <= list.right(),
            "MongoDB 查询历史记录行不能越出列表"
        );
        assert!(
            actions.size.width > px(0.0),
            "MongoDB 查询历史操作组不能为零宽"
        );
        assert!(
            actions.right() <= row.right(),
            "MongoDB 查询历史操作组不能越出记录行"
        );
        for (name, bounds) in &toolbar_children {
            assert!(
                bounds.size.width > px(0.0),
                "MongoDB 查询历史{name}不能为零宽"
            );
            assert!(
                bounds.origin.x >= toolbar.origin.x,
                "MongoDB 查询历史{name}不能越出工具栏左侧"
            );
            assert!(
                bounds.right() <= toolbar.right(),
                "MongoDB 查询历史{name}不能越出工具栏右侧"
            );
            assert!(
                bounds.origin.y >= toolbar.origin.y,
                "MongoDB 查询历史{name}不能越出工具栏上侧"
            );
            assert!(
                bounds.bottom() <= toolbar.bottom(),
                "MongoDB 查询历史{name}不能被工具栏裁切"
            );
        }
        for (index, (left_name, left)) in toolbar_children.iter().enumerate() {
            for (right_name, right) in toolbar_children.iter().skip(index + 1) {
                let separated = left.right() <= right.origin.x
                    || right.right() <= left.origin.x
                    || left.bottom() <= right.origin.y
                    || right.bottom() <= left.origin.y;
                assert!(
                    separated,
                    "MongoDB 查询历史工具栏子项不能重叠：{left_name} / {right_name}"
                );
            }
        }
        assert!(
            list.bottom() <= px(height),
            "MongoDB 查询历史列表不能越出窗口底部"
        );

        cx.update(|window, app| window.close_dialog(app));
        cx.run_until_parked();
    }
}
