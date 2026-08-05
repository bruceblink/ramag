//! 同步预检、确认门禁和执行生命周期入口。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use ramag_domain::entities::{
    ConnectionConfig, DataSyncRequest, DataSyncSummary, DataSyncTaskId, DriverKind,
    SyncObjectMapping, SyncPlannedObject, SyncTargetFingerprint,
};
use ramag_domain::error::{DomainError, Result};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::gate::{DataSyncExecutionContext, DataSyncGate, DataSyncPermit};
use super::mongo_preflight::preflight_mongo;
use super::mongo_sync::run_mongo_sync;
use super::sql_preflight::preflight_sql;
use super::sql_sync::run_sql_sync;
use crate::usecases::{ConnectionService, MongoService};

/// 目录选择器最多保留的对象数。完整范围仍可用“全部对象”同步，避免大库选择器无界占用内存。
pub const MAX_DATA_SYNC_CATALOG_OBJECTS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSyncObjectCatalog {
    pub names: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSyncConfirmation {
    CreateMissingTargets,
    ContinueWithExistingTargets,
}

#[derive(Debug, Clone)]
pub struct DataSyncPreflightReport {
    pub task_id: DataSyncTaskId,
    pub engine: DriverKind,
    pub source_connection: String,
    pub source_scope: String,
    pub source_version: String,
    pub target_connection: String,
    pub target_scope: String,
    pub target_version: String,
    pub objects: Vec<SyncPlannedObject>,
    pub objects_total: Option<u64>,
    pub target_scope_exists: bool,
    pub requires_second_confirmation: bool,
    pub target_fingerprint: SyncTargetFingerprint,
    pub warnings: Vec<String>,
    pub warnings_overflow: u64,
}

impl DataSyncPreflightReport {
    pub(super) fn push_warning(&mut self, warning: impl Into<String>) {
        if self.warnings.len() < ramag_domain::entities::MAX_TRANSFER_WARNINGS {
            self.warnings.push(warning.into());
        } else {
            self.warnings_overflow = self.warnings_overflow.saturating_add(1);
        }
    }
}

/// 含运行期连接快照，因此刻意不实现 Debug / Serialize，避免凭据泄露。
pub struct PreparedDataSync {
    pub(super) request: DataSyncRequest,
    pub(super) source: ConnectionConfig,
    pub(super) target: ConnectionConfig,
    pub(super) report: DataSyncPreflightReport,
    pub(super) engine_plan: PreparedEnginePlan,
}

impl PreparedDataSync {
    pub fn report(&self) -> &DataSyncPreflightReport {
        &self.report
    }
}

pub struct StartedDataSync {
    pub(super) permit: DataSyncPermit,
    pub(super) prepared: PreparedDataSync,
}

impl StartedDataSync {
    pub fn permit(&self) -> &DataSyncPermit {
        &self.permit
    }
}

pub(super) enum PreparedEnginePlan {
    Mongo(MongoPreparedPlan),
    Sql(SqlPreparedPlan),
}

pub(super) struct SqlPreparedPlan {
    pub scope: ramag_domain::entities::SqlSyncScope,
    pub namespace_exists: bool,
    pub namespace_create: Option<String>,
    pub pre_create_statements: Vec<String>,
    pub postgres_enums: Vec<SqlPreparedEnum>,
    pub objects: Vec<SqlPreparedObject>,
    pub target_snapshot: SqlTargetSnapshot,
}

pub(super) struct SqlPreparedEnum {
    pub name: String,
    pub signature: String,
    pub create_statement: String,
}

pub(super) struct SqlPreparedObject {
    pub mapping: SyncObjectMapping,
    pub identity: ramag_domain::entities::SqlRecordIdentity,
    pub writable_columns: Vec<String>,
    pub source_text_columns: HashSet<String>,
    pub target_exists: bool,
    pub create_statement: Option<String>,
    pub post_create_statements: Vec<String>,
    pub final_statements: Vec<String>,
    pub has_identity_always: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SqlTargetSnapshot {
    pub namespace_exists: bool,
    pub enum_types: Vec<SqlEnumSnapshot>,
    pub tables: Vec<SqlTableSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SqlEnumSnapshot {
    pub name: String,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SqlTableSnapshot {
    pub name: String,
    pub exists: bool,
    pub columns: Vec<SqlColumnSnapshot>,
    pub indexes: Vec<SqlIndexSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SqlColumnSnapshot {
    pub name: String,
    pub raw_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub generated: bool,
    pub collation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SqlIndexSnapshot {
    pub name: String,
    pub unique: bool,
    pub primary: bool,
    pub columns: Vec<String>,
}

pub(super) struct MongoPreparedPlan {
    pub scope: ramag_domain::entities::MongoSyncScope,
    pub objects: Vec<MongoPreparedObject>,
    pub target_snapshot: MongoTargetSnapshot,
}

pub(super) struct MongoPreparedObject {
    pub mapping: SyncObjectMapping,
    pub source_blueprint: MongoCollectionBlueprint,
    pub missing_indexes: Vec<serde_json::Value>,
    pub target_exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct MongoCollectionBlueprint {
    pub options: serde_json::Value,
    pub indexes: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct MongoTargetSnapshot {
    pub database_exists: bool,
    pub collections: Vec<(String, Option<MongoCollectionBlueprint>)>,
}

pub struct DataSyncService {
    connection_service: Arc<ConnectionService>,
    mongo_service: Arc<MongoService>,
    gate: Arc<DataSyncGate>,
}

impl DataSyncService {
    pub fn new(
        connection_service: Arc<ConnectionService>,
        mongo_service: Arc<MongoService>,
        gate: Arc<DataSyncGate>,
    ) -> Self {
        Self {
            connection_service,
            mongo_service,
            gate,
        }
    }

    pub fn gate(&self) -> &Arc<DataSyncGate> {
        &self.gate
    }

    /// 列出同步范围。SQL 返回 Database / Schema，MongoDB 返回 Database。
    pub async fn list_catalog_scopes(&self, config: &ConnectionConfig) -> Result<Vec<String>> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        let mut scopes: Vec<String> = match config.driver {
            DriverKind::Mysql | DriverKind::Postgres => self
                .connection_service
                .list_schemas(config)
                .await?
                .into_iter()
                .map(|schema| schema.name)
                .filter(|name| !protected_catalog_scope(config.driver, name))
                .collect(),
            DriverKind::Mongodb => self
                .mongo_service
                .list_databases(config)
                .await?
                .into_iter()
                .map(|database| database.name)
                .filter(|name| !protected_catalog_scope(config.driver, name))
                .collect(),
            DriverKind::Redis => {
                return Err(DomainError::InvalidConfig("Redis 不支持数据同步".into()));
            }
        };
        scopes.sort();
        scopes.dedup();
        Ok(scopes)
    }

    /// 列出指定范围中的可同步对象。视图不属于当前同步范围，会在目录阶段过滤。
    pub async fn list_catalog_objects(
        &self,
        config: &ConnectionConfig,
        scope: &str,
    ) -> Result<DataSyncObjectCatalog> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        let mut names = match config.driver {
            DriverKind::Mysql | DriverKind::Postgres => self
                .connection_service
                .list_tables(config, scope)
                .await?
                .into_iter()
                .filter(|table| !table.is_view)
                .map(|table| table.name)
                .collect::<Vec<_>>(),
            DriverKind::Mongodb => self
                .mongo_service
                .list_collections(config, scope)
                .await?
                .into_iter()
                .filter(|collection| !collection.is_view)
                .map(|collection| collection.name)
                .collect::<Vec<_>>(),
            DriverKind::Redis => {
                return Err(DomainError::InvalidConfig("Redis 不支持数据同步".into()));
            }
        };
        names.sort();
        names.dedup();
        let truncated = names.len() > MAX_DATA_SYNC_CATALOG_OBJECTS;
        names.truncate(MAX_DATA_SYNC_CATALOG_OBJECTS);
        Ok(DataSyncObjectCatalog { names, truncated })
    }

    pub async fn preflight(&self, request: DataSyncRequest) -> Result<PreparedDataSync> {
        let source = self
            .connection_service
            .get(&request.source_connection_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("源连接不存在或已被删除".into()))?;
        let target = self
            .connection_service
            .get(&request.target_connection_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("目标连接不存在或已被删除".into()))?;
        request.validate_connections(&source, &target)?;
        reject_obvious_self_sync(&request, &source, &target)?;

        match &request.scope {
            ramag_domain::entities::DataSyncScope::Mongo(scope) => {
                preflight_mongo(self, request.clone(), source, target, scope.clone()).await
            }
            ramag_domain::entities::DataSyncScope::Sql(scope) => {
                preflight_sql(self, request.clone(), source, target, scope.clone()).await
            }
        }
    }

    pub fn start(
        &self,
        prepared: PreparedDataSync,
        confirmation: DataSyncConfirmation,
    ) -> Result<StartedDataSync> {
        let expected = if prepared.report.requires_second_confirmation {
            DataSyncConfirmation::ContinueWithExistingTargets
        } else {
            DataSyncConfirmation::CreateMissingTargets
        };
        if confirmation != expected {
            return Err(DomainError::InvalidConfig(
                if prepared.report.requires_second_confirmation {
                    "目标范围或对象已存在，必须完成二次确认后才能同步".into()
                } else {
                    "同步确认状态与最新预检结果不一致，请重新预检".into()
                },
            ));
        }
        let context = DataSyncExecutionContext {
            source_connection: prepared.report.source_connection.clone(),
            source_scope: prepared.report.source_scope.clone(),
            target_connection: prepared.report.target_connection.clone(),
            target_scope: prepared.report.target_scope.clone(),
        };
        let permit = self
            .gate
            .begin(prepared.request.task_id.clone(), context)
            .ok_or_else(|| DomainError::Forbidden("已有数据同步任务正在进行".into()))?;
        Ok(StartedDataSync { permit, prepared })
    }

    /// 调用前 `start` 已获取应用锁和占屏许可；本方法保证任何结果都进入终态。
    pub async fn execute(&self, started: StartedDataSync) {
        let StartedDataSync { permit, prepared } = started;
        let start = Instant::now();
        let mut summary = DataSyncSummary::default();
        let result = match self.verify_connection_snapshots(&prepared).await {
            Ok(()) => match &prepared.engine_plan {
                PreparedEnginePlan::Mongo(plan) => {
                    run_mongo_sync(self, &prepared, plan, &permit, &mut summary).await
                }
                PreparedEnginePlan::Sql(plan) => {
                    run_sql_sync(self, &prepared, plan, &permit, &mut summary).await
                }
            },
            Err(error) => Err(error),
        };
        summary.elapsed_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        if let Some(snapshot) = self.gate.snapshot()
            && snapshot.task_id == *permit.task_id()
        {
            summary.bytes = summary.bytes.max(snapshot.progress.bytes);
        }
        match result {
            Ok(()) if summary.cancelled => {
                if !self.gate.finish_cancelled(&permit, summary) {
                    tracing::warn!(task_id = %permit.task_id(), "stale sync cancellation result ignored");
                }
            }
            Ok(()) => {
                if !self.gate.finish_completed(&permit, summary) {
                    tracing::warn!(task_id = %permit.task_id(), "stale sync completion result ignored");
                }
            }
            Err(error) => {
                summary.failed = summary.failed.max(1);
                if !self.gate.finish_failed(&permit, summary, error.to_string()) {
                    tracing::warn!(task_id = %permit.task_id(), "stale sync failure result ignored");
                }
            }
        }
    }

    pub fn request_cancel(&self, permit: &DataSyncPermit) -> bool {
        self.gate.request_cancel(permit)
    }

    pub fn acknowledge_result(&self, permit: &DataSyncPermit) -> bool {
        self.gate.acknowledge_and_release(permit)
    }

    async fn verify_connection_snapshots(&self, prepared: &PreparedDataSync) -> Result<()> {
        let source = self
            .connection_service
            .get(&prepared.request.source_connection_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("源连接已在预检后被删除".into()))?;
        let target = self
            .connection_service
            .get(&prepared.request.target_connection_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("目标连接已在预检后被删除".into()))?;
        if source != prepared.source || target != prepared.target {
            return Err(DomainError::InvalidConfig(
                "源或目标连接已在预检后被修改，请重新预检".into(),
            ));
        }
        prepared.request.validate_connections(&source, &target)
    }

    pub(super) fn mongo_service(&self) -> &MongoService {
        &self.mongo_service
    }

    pub(super) fn connection_service(&self) -> &ConnectionService {
        &self.connection_service
    }
}

fn protected_catalog_scope(driver: DriverKind, name: &str) -> bool {
    match driver {
        DriverKind::Mysql => matches!(
            name.to_ascii_lowercase().as_str(),
            "information_schema" | "mysql" | "performance_schema" | "sys"
        ),
        DriverKind::Postgres => {
            let lower = name.to_ascii_lowercase();
            lower == "information_schema"
                || lower == "pg_catalog"
                || lower.starts_with("pg_toast")
                || lower.starts_with("pg_temp_")
        }
        DriverKind::Mongodb => matches!(name, "admin" | "config" | "local"),
        DriverKind::Redis => false,
    }
}

pub(super) fn fingerprint(value: &impl Serialize) -> Result<SyncTargetFingerprint> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| DomainError::Other(format!("生成目标状态指纹失败：{error}")))?;
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    Ok(SyncTargetFingerprint(digest))
}

fn reject_obvious_self_sync(
    request: &DataSyncRequest,
    source: &ConnectionConfig,
    target: &ConnectionConfig,
) -> Result<()> {
    let same_endpoint = source.host.eq_ignore_ascii_case(&target.host)
        && source.port == target.port
        && source.ssh_target == target.ssh_target
        && source.ssh_port == target.ssh_port
        && (source.driver != DriverKind::Postgres || source.database == target.database);
    if !same_endpoint {
        return Ok(());
    }
    let same_mapping = |selection: &ramag_domain::entities::SyncObjectSelection| match selection {
        ramag_domain::entities::SyncObjectSelection::All => true,
        ramag_domain::entities::SyncObjectSelection::Selected(mappings) => mappings
            .iter()
            .all(|mapping| mapping.source == mapping.target),
    };
    let same_scope = match &request.scope {
        ramag_domain::entities::DataSyncScope::Sql(scope) => {
            scope.source_namespace == scope.target_namespace && same_mapping(&scope.tables)
        }
        ramag_domain::entities::DataSyncScope::Mongo(scope) => {
            scope.source_database == scope.target_database && same_mapping(&scope.collections)
        }
    };
    if same_scope {
        return Err(DomainError::InvalidConfig(
            "源和目标连接明显指向同一实例、同一范围且名称未变化，无需同步".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{ConnectionId, DataSyncScope, SqlSyncScope, SyncObjectSelection};

    #[test]
    fn obvious_same_physical_scope_is_rejected_but_rename_is_allowed() {
        let mut source = ConnectionConfig::new_mysql("source", "127.0.0.1", 3306, "root");
        let mut target = source.clone();
        source.id = ConnectionId::new();
        target.id = ConnectionId::new();
        let mut request = DataSyncRequest {
            task_id: DataSyncTaskId::new(),
            source_connection_id: source.id.clone(),
            target_connection_id: target.id.clone(),
            engine: DriverKind::Mysql,
            scope: DataSyncScope::Sql(SqlSyncScope {
                source_namespace: "app".into(),
                target_namespace: "app".into(),
                tables: SyncObjectSelection::All,
            }),
        };
        assert!(reject_obvious_self_sync(&request, &source, &target).is_err());
        if let DataSyncScope::Sql(scope) = &mut request.scope {
            scope.target_namespace = "archive".into();
        }
        assert!(reject_obvious_self_sync(&request, &source, &target).is_ok());
    }

    #[test]
    fn protected_system_scopes_are_not_offered_for_sync() {
        assert!(protected_catalog_scope(DriverKind::Mysql, "mysql"));
        assert!(protected_catalog_scope(DriverKind::Postgres, "pg_catalog"));
        assert!(protected_catalog_scope(DriverKind::Mongodb, "admin"));
        assert!(!protected_catalog_scope(DriverKind::Mysql, "orders"));
        assert!(!protected_catalog_scope(DriverKind::Postgres, "public"));
    }
}
