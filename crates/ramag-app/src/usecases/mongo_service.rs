//! MongoDB 连接与文档服务。

use std::sync::Arc;

use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, InsertManyOutcome, MongoCollection, MongoDatabase,
    MongoDocument, MongoQueryResult, MongoQuerySpec, QueryHistoryPage, QueryRecord,
};
use ramag_domain::error::Result;
use ramag_domain::traits::{DocDriver, Storage};

pub struct MongoService {
    driver: Arc<dyn DocDriver>,
    storage: Arc<dyn Storage>,
}

const HISTORY_INLINE_BYTE_BUDGET: u64 = 32 * 1024 * 1024;

impl MongoService {
    pub fn new(driver: Arc<dyn DocDriver>, storage: Arc<dyn Storage>) -> Self {
        Self { driver, storage }
    }

    pub async fn test(&self, config: &ConnectionConfig) -> Result<()> {
        let started = std::time::Instant::now();
        let result = self.driver.test_connection(config).await;
        match &result {
            Ok(()) => {
                tracing::info!(
                    operation = "mongodb_connection_test",
                    connection_id = %config.id,
                    elapsed_ms = started.elapsed().as_millis(),
                    "mongodb connection test succeeded"
                )
            }
            Err(error) => {
                tracing::warn!(
                    operation = "mongodb_connection_test",
                    error = %error,
                    connection_id = %config.id,
                    elapsed_ms = started.elapsed().as_millis(),
                    "mongodb connection test failed"
                )
            }
        }
        result
    }

    pub async fn server_version(&self, config: &ConnectionConfig) -> Result<String> {
        let result = self.driver.server_version(config).await;
        log_mongo_error("mongodb_server_version", config, None, None, &result);
        result
    }

    pub fn evict_pool(&self, id: &ConnectionId) {
        self.driver.evict_pool(id);
    }

    pub async fn list_databases(&self, config: &ConnectionConfig) -> Result<Vec<MongoDatabase>> {
        let result = retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.list_databases(config).await
        );
        let result = match result {
            Ok(databases) => crate::run_blocking(move || Ok(sort_databases(databases))).await,
            Err(error) => Err(error),
        };
        log_mongo_error("mongodb_list_databases", config, None, None, &result);
        result
    }

    pub async fn list_collections(
        &self,
        config: &ConnectionConfig,
        db: &str,
    ) -> Result<Vec<MongoCollection>> {
        let result = retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.list_collections(config, db).await
        );
        log_mongo_error("mongodb_list_collections", config, Some(db), None, &result);
        result
    }

    pub async fn find(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        spec: &MongoQuerySpec,
    ) -> Result<MongoQueryResult> {
        let result = retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.find(config, db, coll, spec).await
        );
        log_mongo_result(config, db, coll, "find", &result);
        result
    }

    pub async fn count(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
    ) -> Result<u64> {
        let result = retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.count(config, db, coll, filter).await
        );
        log_mongo_error("mongodb_count", config, Some(db), Some(coll), &result);
        result
    }

    pub async fn insert_many(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        documents: Vec<MongoDocument>,
        skip_duplicates: bool,
    ) -> Result<InsertManyOutcome> {
        let document_count = documents.len();
        let result = self
            .driver
            .insert_many(config, db, coll, documents, skip_duplicates)
            .await;
        match &result {
            Ok(outcome) => {
                tracing::info!(
                    operation = "mongodb_insert_many",
                    connection_id = %config.id,
                    db,
                    collection = coll,
                    document_count,
                    inserted = outcome.inserted,
                    duplicates = outcome.duplicates,
                    "mongodb insert many completed"
                )
            }
            Err(error) => {
                tracing::warn!(
                    operation = "mongodb_insert_many",
                    error = %error,
                    connection_id = %config.id,
                    db,
                    collection = coll,
                    document_count,
                    "mongodb insert many failed"
                )
            }
        }
        result
    }

    pub async fn insert_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        document: MongoDocument,
    ) -> Result<String> {
        let result = self.driver.insert_one(config, db, coll, document).await;
        match &result {
            Ok(_) => {
                tracing::info!(
                    operation = "mongodb_insert_one",
                    connection_id = %config.id,
                    db,
                    collection = coll,
                    "mongodb insert one completed"
                )
            }
            Err(error) => {
                tracing::warn!(
                    operation = "mongodb_insert_one",
                    error = %error,
                    connection_id = %config.id,
                    db,
                    collection = coll,
                    "mongodb insert one failed"
                )
            }
        }
        result
    }

    pub async fn update_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
        update: &MongoDocument,
    ) -> Result<MongoQueryResult> {
        let result = self
            .driver
            .update_one(config, db, coll, filter, update)
            .await;
        log_mongo_result(config, db, coll, "update_one", &result);
        result
    }

    pub async fn delete_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
    ) -> Result<MongoQueryResult> {
        let result = self.driver.delete_one(config, db, coll, filter).await;
        log_mongo_result(config, db, coll, "delete_one", &result);
        result
    }

    pub async fn run_command(
        &self,
        config: &ConnectionConfig,
        db: &str,
        command: MongoDocument,
    ) -> Result<MongoDocument> {
        let command_name = safe_mongo_command_name(&command);
        let result = self.driver.run_command(config, db, command).await;
        match &result {
            Ok(_) => {
                tracing::info!(
                    operation = "mongodb_command",
                    connection_id = %config.id,
                    db,
                    command = %command_name,
                    "mongodb command completed"
                )
            }
            Err(error) => {
                tracing::warn!(
                    operation = "mongodb_command",
                    error = %error,
                    connection_id = %config.id,
                    db,
                    command = %command_name,
                    "mongodb command failed"
                )
            }
        }
        result
    }

    // 历史与 SQL 共用存储，sql 保存 JSON 命令。

    pub async fn append_history(
        &self,
        config: &ConnectionConfig,
        command_text: String,
        result: &Result<MongoQueryResult>,
    ) {
        let record = match result {
            Ok(r) => {
                let rows = if r.documents.is_empty() {
                    r.affected
                } else {
                    r.documents.len() as u64
                };
                QueryRecord::new_success(
                    config.id.clone(),
                    &config.name,
                    &command_text,
                    r.elapsed_ms,
                    rows,
                )
            }
            Err(e) => QueryRecord::new_failed(
                config.id.clone(),
                &config.name,
                &command_text,
                e.to_string(),
            ),
        };
        if let Err(e) = self.storage.append_history(&record).await {
            tracing::warn!(
                operation = "mongodb_query_history_append",
                error = %e,
                connection_id = %config.id,
                command_bytes = command_text.len(),
                "append mongodb query history failed"
            );
        }
    }

    /// 按连接列出查询历史。
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
                operation = "mongodb_query_history_list",
                error = %error,
                connection_id = ?connection_id,
                limit,
                "list mongodb query history failed"
            );
        }
        result
    }

    pub async fn delete_history(&self, id: &ramag_domain::entities::QueryRecordId) -> Result<()> {
        let result = self.storage.delete_history(id).await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "mongodb_query_history_delete",
                record_id = %id,
                "mongodb query history deleted"
            ),
            Err(error) => tracing::warn!(
                operation = "mongodb_query_history_delete",
                error = %error,
                record_id = %id,
                "delete mongodb query history failed"
            ),
        }
        result
    }

    /// 清空指定连接的历史；`None` 表示全部连接。
    pub async fn clear_history(&self, connection_id: Option<&ConnectionId>) -> Result<()> {
        let result = self.storage.clear_history(connection_id).await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "mongodb_query_history_clear",
                connection_id = ?connection_id,
                "mongodb query history cleared"
            ),
            Err(error) => tracing::warn!(
                operation = "mongodb_query_history_clear",
                error = %error,
                connection_id = ?connection_id,
                "clear mongodb query history failed"
            ),
        }
        result
    }
}

fn log_mongo_error<T>(
    operation: &'static str,
    config: &ConnectionConfig,
    db: Option<&str>,
    collection: Option<&str>,
    result: &Result<T>,
) {
    if let Err(error) = result {
        tracing::warn!(
            operation,
            error = %error,
            connection_id = %config.id,
            db = db.unwrap_or("-"),
            collection = collection.unwrap_or("-"),
            "mongodb operation failed"
        );
    }
}

fn sort_databases(mut databases: Vec<MongoDatabase>) -> Vec<MongoDatabase> {
    databases.sort_by(|left, right| left.name.cmp(&right.name));
    databases
}

fn log_mongo_result(
    config: &ConnectionConfig,
    db: &str,
    collection: &str,
    operation: &'static str,
    result: &Result<MongoQueryResult>,
) {
    match result {
        Ok(output) => tracing::info!(
            connection_id = %config.id,
            db,
            collection,
            operation,
            documents = output.documents.len(),
            affected = output.affected,
            elapsed_ms = output.elapsed_ms,
            truncated = output.truncated,
            "mongodb operation completed"
        ),
        Err(error) => tracing::warn!(
            error = %error,
            connection_id = %config.id,
            db,
            collection,
            operation,
            "mongodb operation failed"
        ),
    }
}

fn safe_mongo_command_name(command: &MongoDocument) -> String {
    command
        .as_object()
        .and_then(|fields| fields.keys().next())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .cloned()
        .unwrap_or_else(|| "invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mongodb_log_command_name_never_keeps_payloads_or_controls() {
        assert_eq!(
            safe_mongo_command_name(&serde_json::json!({"find": "users"})),
            "find"
        );
        assert_eq!(
            safe_mongo_command_name(&serde_json::json!({"bad\nname": "secret"})),
            "invalid"
        );
        assert_eq!(
            safe_mongo_command_name(&serde_json::json!(["find", "users"])),
            "invalid"
        );
    }

    #[test]
    fn database_results_are_sorted_for_all_driver_implementations() {
        let databases = vec![
            MongoDatabase {
                name: "users".into(),
                size_on_disk: None,
                empty: false,
            },
            MongoDatabase {
                name: "admin".into(),
                size_on_disk: None,
                empty: false,
            },
        ];

        assert_eq!(sort_databases(databases)[0].name, "admin");
    }
}
