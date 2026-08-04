//! 同步预检、确认门禁和执行生命周期入口。

use std::sync::Arc;
use std::time::Instant;

use ramag_domain::entities::{
    ConnectionConfig, DataSyncRequest, DataSyncSummary, DataSyncTaskId, DriverKind, RedisSyncScope,
    SyncObjectMapping, SyncObjectState, SyncPlannedObject, SyncTargetFingerprint,
};
use ramag_domain::error::{DomainError, Result};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::gate::{DataSyncExecutionContext, DataSyncGate, DataSyncPermit};
use super::mongo_preflight::preflight_mongo;
use super::mongo_sync::run_mongo_sync;
use super::redis_sync::{redis_literal_prefix_pattern, run_redis_sync};
use super::sql_preflight::preflight_sql;
use super::sql_sync::run_sql_sync;
use crate::usecases::{ConnectionService, MongoService, RedisService};

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
    Redis(RedisPreparedPlan),
    Mongo(MongoPreparedPlan),
    Sql(SqlPreparedPlan),
}

pub(super) struct SqlPreparedPlan {
    pub scope: ramag_domain::entities::SqlSyncScope,
    pub namespace_exists: bool,
    pub namespace_create: Option<String>,
    pub pre_create_statements: Vec<String>,
    pub objects: Vec<SqlPreparedObject>,
    pub target_snapshot: SqlTargetSnapshot,
}

pub(super) struct SqlPreparedObject {
    pub mapping: SyncObjectMapping,
    pub identity: ramag_domain::entities::SqlRecordIdentity,
    pub writable_columns: Vec<String>,
    pub target_exists: bool,
    pub create_statement: Option<String>,
    pub post_create_statements: Vec<String>,
    pub final_statements: Vec<String>,
    pub has_identity_always: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SqlTargetSnapshot {
    pub namespace_exists: bool,
    pub tables: Vec<SqlTableSnapshot>,
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

pub(super) struct RedisPreparedPlan {
    pub scope: RedisSyncScope,
    pub target_snapshot: RedisTargetSnapshot,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) enum RedisTargetSnapshot {
    Database {
        db: u8,
        size: u64,
    },
    Prefix {
        db: u8,
        prefix: String,
        has_any: bool,
    },
    Keys {
        db: u8,
        states: Vec<(String, bool)>,
    },
}

pub struct DataSyncService {
    connection_service: Arc<ConnectionService>,
    redis_service: Arc<RedisService>,
    mongo_service: Arc<MongoService>,
    gate: Arc<DataSyncGate>,
}

impl DataSyncService {
    pub fn new(
        connection_service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        gate: Arc<DataSyncGate>,
    ) -> Self {
        Self {
            connection_service,
            redis_service,
            mongo_service,
            gate,
        }
    }

    pub fn gate(&self) -> &Arc<DataSyncGate> {
        &self.gate
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
            ramag_domain::entities::DataSyncScope::Redis(scope) => {
                self.preflight_redis(request.clone(), source, target, scope.clone())
                    .await
            }
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
                    "目标已有数据，必须完成二次确认后才能同步".into()
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
                PreparedEnginePlan::Redis(plan) => {
                    run_redis_sync(self, &prepared, plan, &permit, &mut summary).await
                }
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

    async fn preflight_redis(
        &self,
        request: DataSyncRequest,
        source: ConnectionConfig,
        target: ConnectionConfig,
        scope: RedisSyncScope,
    ) -> Result<PreparedDataSync> {
        let (source_version, target_version) = futures::join!(
            self.redis_service.server_version(&source),
            self.redis_service.server_version(&target)
        );
        let source_version = source_version.map_err(|error| {
            DomainError::ConnectionFailed(format!("源 Redis 连接预检失败：{}", error.message()))
        })?;
        let target_version = target_version.map_err(|error| {
            DomainError::ConnectionFailed(format!("目标 Redis 连接预检失败：{}", error.message()))
        })?;

        let (target_snapshot, objects, objects_total, mut warnings) =
            self.inspect_redis_ranges(&source, &target, &scope).await?;
        let requires_second_confirmation = match &target_snapshot {
            RedisTargetSnapshot::Database { size, .. } => *size > 0,
            RedisTargetSnapshot::Prefix { has_any, .. } => *has_any,
            RedisTargetSnapshot::Keys { states, .. } => states.iter().any(|(_, exists)| *exists),
        };
        let fingerprint = fingerprint(&target_snapshot)?;
        let mut report = DataSyncPreflightReport {
            task_id: request.task_id.clone(),
            engine: DriverKind::Redis,
            source_connection: source.name.clone(),
            source_scope: redis_source_scope_label(&scope),
            source_version,
            target_connection: target.name.clone(),
            target_scope: redis_target_scope_label(&scope),
            target_version,
            objects,
            objects_total,
            requires_second_confirmation,
            target_fingerprint: fingerprint,
            warnings: Vec::new(),
            warnings_overflow: 0,
        };
        for warning in warnings.drain(..) {
            report.push_warning(warning);
        }
        Ok(PreparedDataSync {
            request,
            source,
            target,
            report,
            engine_plan: PreparedEnginePlan::Redis(RedisPreparedPlan {
                scope,
                target_snapshot,
            }),
        })
    }

    async fn inspect_redis_ranges(
        &self,
        source: &ConnectionConfig,
        target: &ConnectionConfig,
        scope: &RedisSyncScope,
    ) -> Result<(
        RedisTargetSnapshot,
        Vec<SyncPlannedObject>,
        Option<u64>,
        Vec<String>,
    )> {
        match scope {
            RedisSyncScope::Database {
                source_db,
                target_db,
                target_prefix,
            } => {
                let (source_size, target_size) = futures::join!(
                    self.redis_service.db_size(source, *source_db),
                    self.redis_service.db_size(target, *target_db)
                );
                let source_size = source_size?;
                let target_size = target_size?;
                let warnings = (source_size == 0)
                    .then(|| "源 Redis DB 当前为空".to_string())
                    .into_iter()
                    .collect();
                Ok((
                    RedisTargetSnapshot::Database {
                        db: *target_db,
                        size: target_size,
                    },
                    vec![SyncPlannedObject {
                        mapping: SyncObjectMapping {
                            source: format!("DB {source_db}"),
                            target: if target_prefix.is_empty() {
                                format!("DB {target_db}")
                            } else {
                                format!("DB {target_db} / 前缀 {target_prefix}")
                            },
                        },
                        state: if target_size == 0 {
                            SyncObjectState::Missing
                        } else {
                            SyncObjectState::ExistingCompatible
                        },
                    }],
                    Some(source_size),
                    warnings,
                ))
            }
            RedisSyncScope::Prefix {
                source_db,
                target_db,
                source_prefix,
                target_prefix,
            } => {
                let source_pattern = redis_literal_prefix_pattern(source_prefix);
                let target_pattern = redis_literal_prefix_pattern(target_prefix);
                let (source_has_any, target_has_any) = futures::join!(
                    self.redis_range_has_any(source, *source_db, &source_pattern),
                    self.redis_range_has_any(target, *target_db, &target_pattern)
                );
                let source_has_any = source_has_any?;
                let target_has_any = target_has_any?;
                let warnings = (!source_has_any)
                    .then(|| "源 Redis 前缀当前没有 Key".to_string())
                    .into_iter()
                    .collect();
                Ok((
                    RedisTargetSnapshot::Prefix {
                        db: *target_db,
                        prefix: target_prefix.clone(),
                        has_any: target_has_any,
                    },
                    vec![SyncPlannedObject {
                        mapping: SyncObjectMapping {
                            source: format!("DB {source_db} / {source_prefix}*"),
                            target: format!("DB {target_db} / {target_prefix}*"),
                        },
                        state: if target_has_any {
                            SyncObjectState::ExistingCompatible
                        } else {
                            SyncObjectState::Missing
                        },
                    }],
                    None,
                    warnings,
                ))
            }
            RedisSyncScope::Keys {
                source_db,
                target_db,
                mappings,
            } => {
                let source_keys: Vec<String> = mappings
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .collect();
                let target_keys: Vec<String> = mappings
                    .iter()
                    .map(|mapping| mapping.target.clone())
                    .collect();
                let (source_states, target_states) = futures::join!(
                    self.redis_service
                        .keys_exist(source, *source_db, &source_keys),
                    self.redis_service
                        .keys_exist(target, *target_db, &target_keys)
                );
                let source_states = source_states?;
                let target_states = target_states?;
                let mut warnings = Vec::new();
                for (mapping, exists) in mappings.iter().zip(&source_states) {
                    if !exists {
                        warnings.push(format!("源 Key 不存在，将跳过：{}", mapping.source));
                    }
                }
                let objects = mappings
                    .iter()
                    .zip(&target_states)
                    .map(|(mapping, exists)| SyncPlannedObject {
                        mapping: SyncObjectMapping {
                            source: mapping.source.clone(),
                            target: mapping.target.clone(),
                        },
                        state: if *exists {
                            SyncObjectState::ExistingCompatible
                        } else {
                            SyncObjectState::Missing
                        },
                    })
                    .collect();
                Ok((
                    RedisTargetSnapshot::Keys {
                        db: *target_db,
                        states: target_keys.into_iter().zip(target_states).collect(),
                    },
                    objects,
                    Some(mappings.len() as u64),
                    warnings,
                ))
            }
        }
    }

    pub(super) async fn current_redis_target_snapshot(
        &self,
        target: &ConnectionConfig,
        scope: &RedisSyncScope,
    ) -> Result<RedisTargetSnapshot> {
        match scope {
            RedisSyncScope::Database { target_db, .. } => Ok(RedisTargetSnapshot::Database {
                db: *target_db,
                size: self.redis_service.db_size(target, *target_db).await?,
            }),
            RedisSyncScope::Prefix {
                target_db,
                target_prefix,
                ..
            } => Ok(RedisTargetSnapshot::Prefix {
                db: *target_db,
                prefix: target_prefix.clone(),
                has_any: self
                    .redis_range_has_any(
                        target,
                        *target_db,
                        &redis_literal_prefix_pattern(target_prefix),
                    )
                    .await?,
            }),
            RedisSyncScope::Keys {
                target_db,
                mappings,
                ..
            } => {
                let keys: Vec<String> = mappings
                    .iter()
                    .map(|mapping| mapping.target.clone())
                    .collect();
                let states = self
                    .redis_service
                    .keys_exist(target, *target_db, &keys)
                    .await?;
                Ok(RedisTargetSnapshot::Keys {
                    db: *target_db,
                    states: keys.into_iter().zip(states).collect(),
                })
            }
        }
    }

    async fn redis_range_has_any(
        &self,
        config: &ConnectionConfig,
        db: u8,
        pattern: &str,
    ) -> Result<bool> {
        let mut cursor = 0u64;
        loop {
            let page = self
                .redis_service
                .scan_batch(config, db, cursor, Some(pattern), None, 5_000)
                .await?;
            if !page.keys.is_empty() {
                return Ok(true);
            }
            if page.cursor == 0 {
                return Ok(false);
            }
            if page.cursor == cursor {
                return Err(DomainError::QueryFailed(
                    "Redis SCAN 游标未推进，无法完成范围预检".into(),
                ));
            }
            cursor = page.cursor;
        }
    }

    pub(super) fn redis_service(&self) -> &RedisService {
        &self.redis_service
    }

    pub(super) fn mongo_service(&self) -> &MongoService {
        &self.mongo_service
    }

    pub(super) fn connection_service(&self) -> &ConnectionService {
        &self.connection_service
    }
}

pub(super) fn fingerprint(value: &impl Serialize) -> Result<SyncTargetFingerprint> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| DomainError::Other(format!("生成目标状态指纹失败：{error}")))?;
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    Ok(SyncTargetFingerprint(digest))
}

fn redis_source_scope_label(scope: &RedisSyncScope) -> String {
    match scope {
        RedisSyncScope::Database { source_db, .. } => format!("DB {source_db}"),
        RedisSyncScope::Prefix {
            source_db,
            source_prefix,
            ..
        } => format!("DB {source_db} / {source_prefix}*"),
        RedisSyncScope::Keys {
            source_db,
            mappings,
            ..
        } => format!("DB {source_db} / {} 个 Key", mappings.len()),
    }
}

fn redis_target_scope_label(scope: &RedisSyncScope) -> String {
    match scope {
        RedisSyncScope::Database {
            target_db,
            target_prefix,
            ..
        } if target_prefix.is_empty() => format!("DB {target_db}"),
        RedisSyncScope::Database {
            target_db,
            target_prefix,
            ..
        } => format!("DB {target_db} / 前缀 {target_prefix}"),
        RedisSyncScope::Prefix {
            target_db,
            target_prefix,
            ..
        } => format!("DB {target_db} / {target_prefix}*"),
        RedisSyncScope::Keys {
            target_db,
            mappings,
            ..
        } => format!("DB {target_db} / {} 个 Key", mappings.len()),
    }
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
    if let ramag_domain::entities::DataSyncScope::Redis(scope) = &request.scope
        && scope.source_db() == scope.target_db()
        && !matches!(scope, RedisSyncScope::Keys { .. })
    {
        return Err(DomainError::InvalidConfig(
            "源和目标连接指向同一 Redis 实例与 DB；扫描范围同步可能读到任务临时 Key，请改用不同 DB 或指定 Key 范围"
                .into(),
        ));
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
        ramag_domain::entities::DataSyncScope::Redis(scope) => match scope {
            RedisSyncScope::Database {
                source_db,
                target_db,
                target_prefix,
            } => source_db == target_db && target_prefix.is_empty(),
            RedisSyncScope::Prefix {
                source_db,
                target_db,
                source_prefix,
                target_prefix,
            } => source_db == target_db && source_prefix == target_prefix,
            RedisSyncScope::Keys {
                source_db,
                target_db,
                mappings,
            } => {
                source_db == target_db
                    && mappings
                        .iter()
                        .all(|mapping| mapping.source == mapping.target)
            }
        },
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
    fn redis_scanned_range_cannot_target_the_same_physical_database() {
        let mut source = ConnectionConfig::new_redis("source", "127.0.0.1", 6379);
        let mut target = source.clone();
        source.id = ConnectionId::new();
        target.id = ConnectionId::new();
        let request = DataSyncRequest {
            task_id: DataSyncTaskId::new(),
            source_connection_id: source.id.clone(),
            target_connection_id: target.id.clone(),
            engine: DriverKind::Redis,
            scope: DataSyncScope::Redis(RedisSyncScope::Database {
                source_db: 0,
                target_db: 0,
                target_prefix: "backup:".into(),
            }),
        };
        let error = reject_obvious_self_sync(&request, &source, &target)
            .expect_err("同 DB 扫描会与同步临时 Key 相互污染");
        assert!(error.message().contains("临时 Key"));
    }
}
