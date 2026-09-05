use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AppContext as _, TestAppContext, VisualTestContext, px, size};
use gpui_component::Root;
use ramag_app::{ConnectionService, MongoService, RedisService};
use ramag_domain::entities::{ConnectionConfig, ConnectionId, QueryRecord, QueryRecordId};
use ramag_domain::error::Result;
use ramag_domain::traits::{Driver, Storage};

use super::ConnectionFormPanel;

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

    async fn delete_history(&self, _id: &QueryRecordId) -> Result<()> {
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

fn services() -> (Arc<ConnectionService>, Arc<RedisService>, Arc<MongoService>) {
    let storage: Arc<dyn Storage> = Arc::new(NoopStorage);
    let mut drivers: HashMap<ramag_domain::entities::DriverKind, Arc<dyn Driver>> = HashMap::new();
    drivers.insert(
        ramag_domain::entities::DriverKind::Sqlite,
        Arc::new(ramag_infra_sqlite::SqliteDriver::new()),
    );
    (
        Arc::new(ConnectionService::new(drivers, storage.clone())),
        Arc::new(RedisService::new(
            Arc::new(ramag_infra_redis::RedisDriver::new()),
            storage.clone(),
        )),
        Arc::new(MongoService::new(
            Arc::new(ramag_infra_mongodb::MongoDriver::new()),
            storage,
        )),
    )
}

fn add_form(cx: &mut TestAppContext, sqlite: bool) -> &mut VisualTestContext {
    let (service, redis_service, mongo_service) = services();
    let (_, visual_cx) = cx.add_window_view(move |window, cx| {
        let form = if sqlite {
            cx.new(|cx| {
                ConnectionFormPanel::new_edit(
                    service,
                    redis_service,
                    mongo_service,
                    ConnectionConfig::new_sqlite("本地数据库", "./data/app.db"),
                    window,
                    cx,
                )
            })
        } else {
            cx.new(|cx| {
                ConnectionFormPanel::new_create(service, redis_service, mongo_service, window, cx)
            })
        };
        Root::new(form, window, cx)
    });
    visual_cx
}

fn assert_inside(
    parent: gpui::Bounds<gpui::Pixels>,
    child: gpui::Bounds<gpui::Pixels>,
    label: &str,
) {
    assert!(
        child.origin.x >= parent.origin.x && child.right() <= parent.right(),
        "{label} 横向越出父容器：parent={parent:?}, child={child:?}"
    );
}

#[gpui::test]
fn driver_selector_wraps_long_labels_inside_narrow_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let visual_cx = add_form(cx, false);

    for width in [360.0, 1024.0, 1440.0] {
        visual_cx.simulate_resize(size(px(width), px(620.0)));
        visual_cx.run_until_parked();

        let selector = visual_cx
            .debug_bounds("conn-form-driver-selector")
            .expect("数据库类型选择器应渲染");
        let body = visual_cx
            .debug_bounds("conn-form-body")
            .expect("连接表单主体应渲染");
        assert_inside(body, selector, "数据库类型选择器");

        let mut buttons = Vec::new();
        for (driver, driver_id) in [
            ("mysql", "driver-btn-mysql"),
            ("postgres", "driver-btn-postgres"),
            ("sqlite", "driver-btn-sqlite"),
            ("redis", "driver-btn-redis"),
            ("mongodb", "driver-btn-mongodb"),
        ] {
            let button = visual_cx
                .debug_bounds(driver_id)
                .expect("数据库类型按钮应渲染");
            assert_inside(selector, button, driver);
            assert!(button.size.width > px(0.0), "{driver} 按钮不能为零宽");
            buttons.push(button);
        }
        assert!(
            buttons
                .iter()
                .any(|button| button.origin.y > buttons[0].origin.y)
                || width >= 500.0,
            "窄窗口应允许数据库类型按钮换行：buttons={buttons:?}"
        );
    }
}

#[gpui::test]
fn sqlite_file_fields_and_actions_stay_inside_supported_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let visual_cx = add_form(cx, true);

    for (width, height) in [(360.0, 620.0), (1024.0, 720.0), (1440.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();

        let body = visual_cx
            .debug_bounds("conn-form-body")
            .expect("连接表单主体应渲染");
        let uri = visual_cx
            .debug_bounds("conn-form-uri-row")
            .expect("SQLite URI 行应渲染");
        let fields = visual_cx
            .debug_bounds("conn-form-sqlite-fields")
            .expect("SQLite 文件字段行应渲染");
        let help = visual_cx
            .debug_bounds("conn-form-sqlite-help")
            .expect("SQLite 提示应渲染");
        let footer = visual_cx
            .debug_bounds("conn-form-footer")
            .expect("连接表单操作区应渲染");
        assert_inside(body, uri, "SQLite URI 行");
        assert_inside(body, fields, "SQLite 文件字段行");
        assert_inside(body, help, "SQLite 提示");
        assert!(footer.right() <= px(width), "底部操作区不能越出窗口");

        for action in ["test", "cancel", "save"] {
            let action_bounds = visual_cx
                .debug_bounds(action)
                .expect("连接表单操作按钮应渲染");
            assert_inside(footer, action_bounds, action);
        }
    }
}
