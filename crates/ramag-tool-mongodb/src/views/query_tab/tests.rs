use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, px, size};
use ramag_app::MongoService;
use ramag_domain::entities::{ConnectionConfig, ConnectionId, QueryRecord, QueryRecordId};
use ramag_domain::error::Result;
use ramag_domain::traits::Storage;

use super::{MongoQueryTab, find_command_template};

#[derive(Default)]
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

#[test]
fn collection_template_escapes_json_string_characters() {
    let template = find_command_template("quotes\"and\\slashes");
    let parsed: serde_json::Value = serde_json::from_str(&template).unwrap();
    assert_eq!(parsed["find"], "quotes\"and\\slashes");
    assert_eq!(parsed["sort"]["_id"], 1);
    assert!(parsed.get("limit").is_none());
}

/// 命令失败时，重试按钮和错误文本在窄窗口内保持可见，并重新走编辑器解析流程。
#[gpui::test]
fn mongo_failure_retry_stays_inside_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        Arc::new(NoopStorage),
    ));
    let config = ConnectionConfig::new_mongodb("失败重试连接", "127.0.0.1", 27017);
    let (tab, cx) = cx.add_window_view(|window, cx| {
        MongoQueryTab::new(
            service,
            config,
            Some("admin".to_string()),
            ramag_ui::ResultMemoryBudget::default(),
            window,
            cx,
        )
    });

    tab.update_in(cx, |tab, window, cx| {
        tab.editor.update(cx, |editor, cx| {
            editor.set_value("{".to_string(), window, cx);
        });
        tab.result.update(cx, |result, cx| {
            result.set_error("MongoDB 连接暂时不可用，请检查连接后重试".into(), cx);
        });
    });

    for width in [360.0, 1024.0, 1440.0] {
        cx.simulate_resize(size(px(width), px(480.0)));
        tab.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();

        let error = cx
            .debug_bounds("mongo-result-error")
            .expect("MongoDB 错误区域应渲染");
        let retry = cx
            .debug_bounds("mongo-result-retry")
            .expect("MongoDB 错误区域应提供重试按钮");
        assert!(error.right() <= px(width), "错误区域不能越出窗口");
        assert!(retry.right() <= error.right(), "重试按钮不能越出错误区域");
        assert!(
            retry.bottom() <= error.bottom(),
            "重试按钮不能被错误区域裁掉"
        );
    }

    let retry = cx
        .debug_bounds("mongo-result-retry")
        .expect("MongoDB 错误区域应提供重试按钮");
    cx.simulate_click(retry.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(tab.read_with(cx, |tab, cx| {
        tab.result
            .read(cx)
            .error
            .as_deref()
            .is_some_and(|message| message.starts_with("JSON 解析失败："))
    }));
}
