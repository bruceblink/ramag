//! PostgreSQL 特有的取消标识记录与空结果列信息读取。主执行流程在 sql-shared。

use std::sync::atomic::Ordering;

use ramag_domain::traits::CancelHandle;
use sqlx::postgres::PgConnection;
use sqlx::{Column as _, Executor, TypeInfo as _};
use tracing::warn;

/// 记录后端进程 ID，供取消查询使用。失败时记录警告，不阻塞查询。
pub async fn record_backend_id(conn: &mut PgConnection, handle: &CancelHandle) {
    match sqlx::query_as::<_, (i32,)>("SELECT pg_backend_pid()")
        .fetch_one(conn)
        .await
    {
        Ok((pid,)) => handle.store(pid as u64, Ordering::SeqCst),
        Err(e) => {
            warn!(operation = "sql_query_cancel", stage = "connection_id", error = %e, "query cancellation id lookup failed")
        }
    }
}

/// 空结果集没有行可供推断时，通过 describe 读取列信息。
pub async fn extract_columns_fallback(
    conn: &mut PgConnection,
    sql: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    match (&mut *conn).describe(sql).await {
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
