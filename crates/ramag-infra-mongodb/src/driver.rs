//! MongoDriver。impl DocDriver。每个方法 clone config + pool 句柄 → run_in_tokio → dispatch 到 metadata / query

use async_trait::async_trait;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DriverKind, MongoCollection, MongoCollectionStats,
    MongoDatabase, MongoDocument, MongoIndex, MongoQueryResult, MongoQuerySpec,
    validate_mongo_collection_name, validate_mongo_database_name, validate_mongo_document,
    validate_mongo_pipeline,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use ramag_domain::traits::DocDriver;
use tracing::warn;

use crate::command::{command_is_write, pipeline_has_write_stage};
use crate::metadata;
use crate::pool::PoolCache;
use crate::query;
use crate::runtime::run_in_tokio;

pub struct MongoDriver {
    pools: PoolCache,
}

impl MongoDriver {
    pub fn new() -> Self {
        Self {
            pools: PoolCache::new(),
        }
    }
}

impl Default for MongoDriver {
    fn default() -> Self {
        Self::new()
    }
}

fn ensure_mongo_config(config: &ConnectionConfig) -> Result<()> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    if config.driver != DriverKind::Mongodb {
        return Err(DomainError::InvalidConfig(format!(
            "MongoDriver 不支持 {:?} 类型连接",
            config.driver
        )));
    }
    Ok(())
}

fn validate_namespace(db: &str, coll: Option<&str>) -> Result<()> {
    validate_mongo_database_name(db)?;
    if let Some(coll) = coll {
        validate_mongo_collection_name(coll)?;
        let namespace_bytes = db
            .len()
            .checked_add(1)
            .and_then(|bytes| bytes.checked_add(coll.len()))
            .ok_or_else(|| DomainError::InvalidConfig("MongoDB namespace 长度溢出".into()))?;
        if namespace_bytes > 255 {
            return Err(DomainError::InvalidConfig(
                "MongoDB database.collection 超过 255 bytes namespace 上限".into(),
            ));
        }
    }
    Ok(())
}

#[async_trait]
impl DocDriver for MongoDriver {
    fn name(&self) -> &'static str {
        "mongodb"
    }

    async fn test_connection(&self, config: &ConnectionConfig) -> Result<()> {
        ensure_mongo_config(config)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::ping(&client).await
        })
        .await
    }

    async fn server_version(&self, config: &ConnectionConfig) -> Result<String> {
        ensure_mongo_config(config)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::server_version(&client).await
        })
        .await
    }

    async fn list_databases(&self, config: &ConnectionConfig) -> Result<Vec<MongoDatabase>> {
        ensure_mongo_config(config)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            metadata::list_databases(&client).await
        })
        .await
    }

    async fn list_collections(
        &self,
        config: &ConnectionConfig,
        db: &str,
    ) -> Result<Vec<MongoCollection>> {
        ensure_mongo_config(config)?;
        validate_namespace(db, None)?;
        let config = config.clone();
        let db = db.to_string();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            metadata::list_collections(&client, &db).await
        })
        .await
    }

    async fn list_indexes(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
    ) -> Result<Vec<MongoIndex>> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            metadata::list_indexes(&client, &db, &coll).await
        })
        .await
    }

    async fn collection_stats(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
    ) -> Result<MongoCollectionStats> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            metadata::collection_stats(&client, &db, &coll).await
        })
        .await
    }

    async fn find(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        spec: &MongoQuerySpec,
    ) -> Result<MongoQueryResult> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        spec.validate()?;
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let spec = spec.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::find(&client, &db, &coll, &spec).await
        })
        .await
    }

    async fn count(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
    ) -> Result<u64> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        if !filter.is_null() {
            validate_mongo_document(filter, "MongoDB count filter")?;
        }
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let filter = filter.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::count(&client, &db, &coll, filter).await
        })
        .await
    }

    async fn aggregate(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        pipeline: Vec<MongoDocument>,
    ) -> Result<MongoQueryResult> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        validate_mongo_pipeline(&pipeline)?;
        if config.production && pipeline_has_write_stage(&pipeline) {
            warn!(conn = %config.name, %db, %coll, "read-only mode: blocked aggregate $out/$merge");
            return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
        }
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::aggregate(&client, &db, &coll, pipeline).await
        })
        .await
    }

    async fn insert_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        document: MongoDocument,
    ) -> Result<String> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        validate_mongo_document(&document, "MongoDB insert document")?;
        if config.production {
            warn!(conn = %config.name, %db, %coll, "read-only mode: blocked insert");
            return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
        }
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::insert_one(&client, &db, &coll, document).await
        })
        .await
    }

    async fn update_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
        update: &MongoDocument,
    ) -> Result<MongoQueryResult> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        validate_mongo_document(filter, "MongoDB update filter")?;
        validate_mongo_document(update, "MongoDB update document")?;
        if config.production {
            warn!(conn = %config.name, %db, %coll, "read-only mode: blocked update");
            return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
        }
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let filter = filter.clone();
        let update = update.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::update_one(&client, &db, &coll, filter, update).await
        })
        .await
    }

    async fn delete_one(
        &self,
        config: &ConnectionConfig,
        db: &str,
        coll: &str,
        filter: &MongoDocument,
    ) -> Result<MongoQueryResult> {
        ensure_mongo_config(config)?;
        validate_namespace(db, Some(coll))?;
        validate_mongo_document(filter, "MongoDB delete filter")?;
        if config.production {
            warn!(conn = %config.name, %db, %coll, "read-only mode: blocked delete");
            return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
        }
        let config = config.clone();
        let db = db.to_string();
        let coll = coll.to_string();
        let filter = filter.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::delete_one(&client, &db, &coll, filter).await
        })
        .await
    }

    async fn run_command(
        &self,
        config: &ConnectionConfig,
        db: &str,
        command: MongoDocument,
    ) -> Result<MongoDocument> {
        ensure_mongo_config(config)?;
        validate_namespace(db, None)?;
        validate_mongo_document(&command, "MongoDB command")?;
        if config.production && command_is_write(&command) {
            warn!(conn = %config.name, %db, "read-only mode: blocked write command");
            return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
        }
        let config = config.clone();
        let db = db.to_string();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let client = pools.get_or_create(&config).await?;
            query::run_command(&client, &db, command).await
        })
        .await
    }

    fn evict_pool(&self, id: &ConnectionId) {
        self.pools.evict(id);
        // 该连接的 SSH 隧道一并关闭（编辑配置后下次建连按新参数重建）
        ramag_infra_tunnel::evict(id);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::validate_namespace;

    #[test]
    fn namespace_validation_runs_before_driver_clones_or_network() {
        assert!(validate_namespace("app", Some("users")).is_ok());
        assert!(validate_namespace("", Some("users")).is_err());
        assert!(validate_namespace("app", Some("")).is_err());
        assert!(validate_namespace("d", Some(&"c".repeat(254))).is_err());
    }
}
