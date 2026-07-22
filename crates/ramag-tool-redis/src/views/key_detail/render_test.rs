//! GPUI 渲染测试：headless 渲染 KeyDetailPanel，用 debug_bounds 断言容器值的
//! uniform_list 行真实拿到了非零布局（回归防护：详情区数据在但视觉空白）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use gpui::{AppContext as _, TestAppContext, px};
use ramag_app::RedisService;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, MAX_REDIS_LOADED_ITEMS, QueryRecord, QueryRecordId, RedisType,
    RedisValue, RedisValueLoad, ScanResult, StreamEntry,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{KvDriver, Storage};

use super::KeyDetailPanel;

/// 空壳 KvDriver：render 是纯展示、不调 driver
#[derive(Default)]
struct MockKv {
    requested_limit: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl KvDriver for MockKv {
    fn name(&self) -> &'static str {
        "mock"
    }
    async fn test_connection(&self, _: &ConnectionConfig) -> Result<()> {
        Ok(())
    }
    async fn server_version(&self, _: &ConnectionConfig) -> Result<String> {
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn db_size(&self, _: &ConnectionConfig, _: u8) -> Result<u64> {
        Ok(0)
    }
    async fn scan(
        &self,
        _: &ConnectionConfig,
        _: u8,
        _: u64,
        _: Option<&str>,
        _: Option<RedisType>,
        _: u32,
    ) -> Result<ScanResult> {
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn key_type(&self, _: &ConnectionConfig, _: u8, _: &str) -> Result<RedisType> {
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn key_ttl(&self, _: &ConnectionConfig, _: u8, _: &str) -> Result<i64> {
        Ok(-1)
    }
    async fn get_value(&self, _: &ConnectionConfig, _: u8, _: &str) -> Result<RedisValue> {
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn get_value_limited(
        &self,
        _: &ConnectionConfig,
        _: u8,
        _: &str,
        limit: usize,
    ) -> Result<RedisValueLoad> {
        if let Some(requested_limit) = self.requested_limit.as_ref() {
            requested_limit.store(limit, Ordering::SeqCst);
            return Ok(RedisValueLoad {
                value: RedisValue::Hash(vec![("field".into(), RedisValue::Text("value".into()))]),
                total: Some(1),
                byte_limited: false,
                memory_warning: false,
            });
        }
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn delete_key(&self, _: &ConnectionConfig, _: u8, _: &str) -> Result<bool> {
        Ok(false)
    }
    async fn set_ttl(&self, _: &ConnectionConfig, _: u8, _: &str, _: Option<i64>) -> Result<bool> {
        Ok(false)
    }
    fn is_write_command(&self, _: &str) -> bool {
        false
    }
    async fn execute_command(
        &self,
        _: &ConnectionConfig,
        _: u8,
        _: Vec<String>,
    ) -> Result<RedisValue> {
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn info(&self, _: &ConnectionConfig, _: &[&str]) -> Result<String> {
        Err(DomainError::NotImplemented("mock".into()))
    }
}

/// 空壳 Storage：render 不调 storage
struct MockStorage;

#[async_trait]
impl Storage for MockStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(vec![])
    }
    async fn get_connection(&self, _: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }
    async fn save_connection(&self, _: &ConnectionConfig) -> Result<()> {
        Ok(())
    }
    async fn delete_connection(&self, _: &ConnectionId) -> Result<()> {
        Ok(())
    }
    async fn append_history(&self, _: &QueryRecord) -> Result<()> {
        Ok(())
    }
    async fn list_history(&self, _: Option<&ConnectionId>, _: usize) -> Result<Vec<QueryRecord>> {
        Ok(vec![])
    }
    async fn delete_history(&self, _: &QueryRecordId) -> Result<()> {
        Ok(())
    }
    async fn clear_history(&self, _: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }
    async fn get_preference(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
    async fn set_preference(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}

fn mock_service() -> Arc<RedisService> {
    Arc::new(RedisService::new(
        Arc::new(MockKv::default()),
        Arc::new(MockStorage),
    ))
}

fn mock_config() -> ConnectionConfig {
    let mut config = ConnectionConfig::new_redis("test", "127.0.0.1", 6379);
    config.password = String::new();
    config
}

#[gpui::test]
fn key_load_uses_global_limit_without_manual_pagination(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let requested_limit = Arc::new(AtomicUsize::new(0));
    let service = Arc::new(RedisService::new(
        Arc::new(MockKv {
            requested_limit: Some(requested_limit.clone()),
        }),
        Arc::new(MockStorage),
    ));

    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| {
            let mut panel = KeyDetailPanel::new(service, cx);
            panel.config = Some(mock_config());
            panel
        });
        panel.update(cx, |panel, cx| panel.load_key("large:hash".into(), cx));
        gpui_component::Root::new(panel, window, cx)
    });
    cx.run_until_parked();

    assert_eq!(
        requested_limit.load(Ordering::SeqCst),
        MAX_REDIS_LOADED_ITEMS
    );
    assert!(cx.debug_bounds("redis-load-more-members").is_none());
}

/// 五种容器类型逐一注入后渲染：类型块必须拿到非零高度布局（回归防护：
/// 数据已加载但详情区视觉空白——flex_grow 在该布局上下文失效导致高度塌缩）
#[gpui::test]
fn container_value_blocks_have_nonzero_bounds(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);

    let text = |s: &str| RedisValue::Text(s.into());
    let cases: Vec<(&'static str, RedisValue)> = vec![
        (
            "redis-zset-block",
            RedisValue::ZSet(vec![(text("alpha"), 1.0), (text("beta"), 2.5)]),
        ),
        (
            "redis-hash-block",
            RedisValue::Hash(vec![("f1".into(), text("v1")), ("f2".into(), text("v2"))]),
        ),
        (
            "redis-list-block",
            RedisValue::List(vec![text("a"), text("b")]),
        ),
        (
            "redis-set-block",
            RedisValue::Set(vec![text("a"), text("b")]),
        ),
        (
            "redis-stream-block",
            RedisValue::Stream(vec![
                StreamEntry {
                    id: "1-0".into(),
                    fields: vec![("k".into(), "v".into())],
                },
                StreamEntry {
                    id: "2-0".into(),
                    fields: vec![("k".into(), "v2".into())],
                },
            ]),
        ),
    ];

    for (selector, value) in cases {
        let (_, cx) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| {
                let mut panel = KeyDetailPanel::new(mock_service(), cx);
                panel.config = Some(mock_config());
                panel.key = Some("k".into());
                panel.value = Some(value.clone());
                panel.collection_total = Some(2);
                panel
            });
            gpui_component::Root::new(panel, window, cx)
        });
        cx.run_until_parked();

        let block = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} 应参与布局"));
        assert!(
            block.size.height > px(8.0),
            "{selector} 高度塌缩：{:?}",
            block.size
        );
        // zset 行级抽查：行高与固定行高一致
        if selector == "redis-zset-block" {
            let row = cx
                .debug_bounds("redis-zset-row-0")
                .expect("zset 行应被渲染");
            assert!(
                row.size.height >= px(30.0),
                "zset 行高度异常：{:?}",
                row.size
            );
        }
    }
}
