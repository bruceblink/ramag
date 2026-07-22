//! MySQL 特有 hook：抓 thread id / SHOW WARNINGS / 空结果列定义。主执行流在 sql-shared

use std::sync::atomic::Ordering;

use ramag_domain::entities::Warning;
use ramag_domain::traits::CancelHandle;
use ramag_infra_sql_shared::MAX_QUERY_WARNINGS;
use sqlx::mysql::MySqlConnection;
use sqlx::{Column as _, Executor, TypeInfo as _};
use tracing::warn;

/// 写入 thread id 到 cancel handle（`KILL QUERY` 需要）。失败仅 warn 不阻塞
pub async fn record_backend_id(conn: &mut MySqlConnection, handle: &CancelHandle) {
    match sqlx::query_as::<_, (u64,)>("SELECT CONNECTION_ID()")
        .fetch_one(conn)
        .await
    {
        Ok((tid,)) => handle.store(tid, Ordering::SeqCst),
        Err(e) => warn!(error = %e, "query cancellation id lookup failed"),
    }
}

/// 抓 SHOW WARNINGS。每条 statement 后立即调（下条会清空 buffer），shared 累加多条
pub async fn fetch_warnings(conn: &mut MySqlConnection) -> Vec<Warning> {
    // 走 prepared 路径。sqlx 0.8 + async_trait + spawn 下 HRTB 不允许 raw_sql，
    // 需要 unsafe transmute 才能避开 1295。退化方案：捕获 1295 静默
    // SHOW WARNINGS 列序固定为 Level / Code / Message；强类型解码避免坏行被静默丢弃。
    // 多取一条作为截断哨兵，shared 层会按整个多语句查询的总预算裁剪并提示。
    let sql = format!("SHOW WARNINGS LIMIT {}", MAX_QUERY_WARNINGS + 1);
    let rows: Result<Vec<(String, u32, String)>, sqlx::Error> =
        sqlx::query_as(&sql).fetch_all(conn).await;
    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|(level, code, message)| Warning {
                level,
                code,
                message,
            })
            .collect(),
        Err(e) => {
            // 1295 = command not supported in prepared statement protocol，老版本服务端限制
            let is_unsupported =
                e.as_database_error().and_then(|d| d.code()).as_deref() == Some("1295");
            if !is_unsupported {
                warn!(error = %e, "query warning lookup failed");
            }
            Vec::new()
        }
    }
}

/// 空结果集 fallback：走 `Connection::describe` 拿 SHOW/DESC 列定义。
/// SHOW TABLES 空结果有列定义，不 fallback 会被 UI 误判为 DML
pub async fn extract_columns_fallback(
    conn: &mut MySqlConnection,
    sql: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    match conn.describe(sql).await {
        Ok(desc) => Some(
            desc.columns
                .iter()
                .map(|c| (c.name().to_string(), c.type_info().name().to_string()))
                .unzip(),
        ),
        Err(e) => {
            warn!(error = %e, "empty-result SQL description failed");
            None
        }
    }
}
