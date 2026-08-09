//! MySQL 的取消标识、查询警告与空结果列定义处理。

use std::sync::atomic::Ordering;

use ramag_domain::entities::Warning;
use ramag_domain::traits::CancelHandle;
use ramag_infra_sql_shared::MAX_QUERY_WARNINGS;
use sqlx::mysql::{MySqlConnection, MySqlDatabaseError};
use sqlx::{Column as _, Executor, TypeInfo as _};
use tracing::warn;

/// 记录 `KILL QUERY` 所需的线程 ID；失败仅记录警告。
pub async fn record_backend_id(conn: &mut MySqlConnection, handle: &CancelHandle) {
    match sqlx::query_as::<_, (u64,)>("SELECT CONNECTION_ID()")
        .fetch_one(conn)
        .await
    {
        Ok((tid,)) => handle.store(tid, Ordering::SeqCst),
        Err(e) => {
            warn!(operation = "sql_query_cancel", stage = "connection_id", error = %e, "query cancellation id lookup failed")
        }
    }
}

/// 读取当前语句的 `SHOW WARNINGS` 结果。
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
            // DatabaseError::code() 返回的是 SQLSTATE（此处为 HY000），需读取 MySQL 错误号。
            let is_unsupported = e
                .as_database_error()
                .and_then(|error| error.try_downcast_ref::<MySqlDatabaseError>())
                .is_some_and(|error| error.number() == 1295);
            if !is_unsupported {
                warn!(operation = "sql_query_warnings", error = %e, "query warning lookup failed");
            }
            Vec::new()
        }
    }
}

/// 通过 `Connection::describe` 获取空结果集的列定义。
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
            warn!(operation = "sql_query_empty_result_description", error = %e, "empty-result SQL description failed");
            None
        }
    }
}
