//! 文档数据库接口。

use async_trait::async_trait;

use crate::entities::{
    ConnectionConfig, ConnectionId, InsertManyOutcome, MongoCollection, MongoCollectionStats,
    MongoDatabase, MongoDocument, MongoIndex, MongoQueryResult, MongoQuerySpec,
};
use crate::error::Result;

#[async_trait]
pub trait DocDriver: Send + Sync {
    /// 用于日志 / UI 显示，如 "mongodb"
    fn name(&self) -> &'static str;

    async fn test_connection(&self, config: &ConnectionConfig) -> Result<()>;

    async fn server_version(&self, config: &ConnectionConfig) -> Result<String>;

    async fn list_databases(&self, config: &ConnectionConfig) -> Result<Vec<MongoDatabase>>;

    async fn list_collections(
        &self,
        config: &ConnectionConfig,
        db: &str,
    ) -> Result<Vec<MongoCollection>>;

    async fn list_indexes(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
    ) -> Result<Vec<MongoIndex>>;

    async fn collection_stats(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
    ) -> Result<MongoCollectionStats>;

    async fn find(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        spec: &MongoQuerySpec,
    ) -> Result<MongoQueryResult>;

    async fn count(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
    ) -> Result<u64>;

    async fn aggregate(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        pipeline: Vec<MongoDocument>,
    ) -> Result<MongoQueryResult>;

    async fn insert_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        document: MongoDocument,
    ) -> Result<String>;

    /// 批量插入（导入用）。`skip_duplicates=true` 走无序批量，重复 `_id`（E11000）
    /// 不算错误只计数；false 走有序批量，任何错误即失败
    async fn insert_many(
        &self,
        _config: &ConnectionConfig,
        _db: &str,
        _coll: &str,
        _documents: Vec<MongoDocument>,
        _skip_duplicates: bool,
    ) -> Result<InsertManyOutcome> {
        Err(crate::error::DomainError::NotImplemented(
            "insert_many".into(),
        ))
    }

    async fn update_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
        update: &MongoDocument,
    ) -> Result<MongoQueryResult>;

    async fn delete_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
    ) -> Result<MongoQueryResult>;

    /// 执行尚未抽象为独立方法的数据库命令。
    async fn run_command(
        &self,
        config: &ConnectionConfig,
        db: &str,
        command: MongoDocument,
    ) -> Result<MongoDocument>;

    /// 配置变更后使对应连接池失效。
    fn evict_pool(&self, _id: &ConnectionId) {}
}
