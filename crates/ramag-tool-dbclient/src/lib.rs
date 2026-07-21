//! DB Client 工具：MySQL / PostgreSQL / Redis 共用入口

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod actions;
pub mod sql_completion;
pub mod views;

pub use actions::{
    CopyCellValue, CopySelectedColumn, ExplainQuery, FindInResults, FormatSql, NewQueryTab,
    RunQuery, RunStatementAtCursor, ToggleSqlEditor,
};
pub use views::DbClientView;
// Redis 命令行控制台快捷键 Action：经本 facade 透传给 bin 绑键（bin 不直接依赖 redis）
pub use ramag_tool_redis::ToggleRedisConsole;

use std::sync::Arc;

use gpui::{App, AppContext as _, Entity, Window};
use ramag_app::{ConnectionService, MongoService, RedisService};
use ramag_domain::traits::{Tool, ToolMeta};

pub fn create_dbclient_view(
    service: Arc<ConnectionService>,
    redis_service: Arc<RedisService>,
    mongo_service: Arc<MongoService>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DbClientView> {
    cx.new(|cx| DbClientView::new(service, redis_service, mongo_service, window, cx))
}

pub struct DbClientTool {
    meta: ToolMeta,
}

impl DbClientTool {
    pub const ID: &'static str = "dbclient";

    pub fn new() -> Self {
        Self {
            meta: ToolMeta::new(
                Self::ID,
                "数据库客户端",
                "MySQL / PostgreSQL / Redis / MongoDB 统一入口",
            )
            .with_icon("database"),
        }
    }
}

impl Default for DbClientTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for DbClientTool {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_correct() {
        let tool = DbClientTool::new();
        assert_eq!(tool.meta().id, "dbclient");
        assert_eq!(tool.meta().name, "数据库客户端");
        assert!(tool.meta().icon.is_some());
    }
}
