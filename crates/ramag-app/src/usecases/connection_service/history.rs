//! SQL 查询历史持久化。

use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, Query, QueryHistoryPage, QueryRecord, QueryRecordId,
    QueryResult,
};
use ramag_domain::error::Result;

use super::{ConnectionService, log_query_result};

const HISTORY_INLINE_BYTE_BUDGET: u64 = 32 * 1024 * 1024;

impl ConnectionService {
    /// 记录已完成的查询，不参与数据库执行；调用方可在更新界面后异步落盘。
    pub async fn append_history(
        &self,
        config: &ConnectionConfig,
        query: &Query,
        result: &Result<QueryResult>,
        cancellable: bool,
    ) {
        log_query_result(config, query, result, cancellable);
        self.append_history_for(config, query, result).await;
    }

    /// 历史写入失败仅记录警告，不阻塞查询。
    pub(super) async fn append_history_for(
        &self,
        config: &ConnectionConfig,
        query: &Query,
        result: &Result<QueryResult>,
    ) {
        let record = match result {
            Ok(output) => QueryRecord::new_success(
                config.id.clone(),
                &config.name,
                &query.sql,
                output.elapsed_ms,
                if output.rows.is_empty() {
                    output.affected_rows
                } else {
                    output.rows.len() as u64
                },
            ),
            Err(error) => QueryRecord::new_failed(
                config.id.clone(),
                &config.name,
                &query.sql,
                error.to_string(),
            ),
        };
        if let Err(error) = self.storage.append_history(&record).await {
            tracing::warn!(
                operation = "sql_query_history_append",
                error = %error,
                connection_id = %config.id,
                query_bytes = query.sql.len(),
                "append query history failed"
            );
        }
    }

    pub async fn list_history(
        &self,
        connection_id: Option<&ConnectionId>,
        limit: usize,
    ) -> Result<QueryHistoryPage> {
        let result = self
            .storage
            .list_history_bounded(connection_id, limit, HISTORY_INLINE_BYTE_BUDGET)
            .await;
        if let Err(error) = &result {
            tracing::warn!(
                operation = "sql_query_history_list",
                error = %error,
                connection_id = ?connection_id,
                limit,
                "list query history failed"
            );
        }
        result
    }

    pub async fn delete_history(&self, id: &QueryRecordId) -> Result<()> {
        let result = self.storage.delete_history(id).await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "sql_query_history_delete",
                record_id = %id,
                "query history deleted"
            ),
            Err(error) => tracing::warn!(
                operation = "sql_query_history_delete",
                error = %error,
                record_id = %id,
                "delete query history failed"
            ),
        }
        result
    }

    /// 清空指定连接的查询历史；`None` 表示全部连接。
    pub async fn clear_history(&self, connection_id: Option<&ConnectionId>) -> Result<()> {
        let result = self.storage.clear_history(connection_id).await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "sql_query_history_clear",
                connection_id = ?connection_id,
                "query history cleared"
            ),
            Err(error) => tracing::warn!(
                operation = "sql_query_history_clear",
                error = %error,
                connection_id = ?connection_id,
                "clear query history failed"
            ),
        }
        result
    }
}
