//! 连接间数据同步实体与纯逻辑校验。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    Column, ConnectionConfig, ConnectionId, DriverKind, Index, MAX_METADATA_ITEMS,
    MAX_TRANSFER_WARNINGS, validate_mongo_collection_name, validate_mongo_database_name,
};
use crate::error::{DomainError, READ_ONLY_MESSAGE, Result};

/// MySQL 的 Database、Table 等标识符最多 64 个字符。
pub const MAX_MYSQL_SYNC_IDENTIFIER_CHARS: usize = 64;
/// PostgreSQL 默认 `NAMEDATALEN=64`，可用标识符最多 63 bytes。
pub const MAX_POSTGRES_SYNC_IDENTIFIER_BYTES: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DataSyncTaskId(pub Uuid);

impl DataSyncTaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DataSyncTaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for DataSyncTaskId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// 一个源对象到一个目标对象的确定性映射。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncObjectMapping {
    pub source: String,
    pub target: String,
}

/// `All` 在预检时展开为当时可见的全部对象，执行计划中不再保留动态范围。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncObjectSelection {
    All,
    Selected(Vec<SyncObjectMapping>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlSyncScope {
    /// MySQL 为 Database，PostgreSQL 为 Schema。
    pub source_namespace: String,
    pub target_namespace: String,
    pub tables: SyncObjectSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MongoSyncScope {
    pub source_database: String,
    pub target_database: String,
    pub collections: SyncObjectSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataSyncScope {
    Sql(SqlSyncScope),
    Mongo(MongoSyncScope),
}

/// 公开请求不包含连接配置快照，避免密码进入序列化、日志或 Debug 输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSyncRequest {
    pub task_id: DataSyncTaskId,
    pub source_connection_id: ConnectionId,
    pub target_connection_id: ConnectionId,
    pub engine: DriverKind,
    pub scope: DataSyncScope,
}

impl DataSyncRequest {
    /// 校验请求与运行期连接快照。目标只读保护在应用层和驱动层还会再次检查。
    pub fn validate_connections(
        &self,
        source: &ConnectionConfig,
        target: &ConnectionConfig,
    ) -> Result<()> {
        if source.id != self.source_connection_id || target.id != self.target_connection_id {
            return Err(DomainError::InvalidConfig(
                "同步请求引用的连接已变化，请重新配置".into(),
            ));
        }
        if source.id == target.id {
            return Err(DomainError::InvalidConfig(
                "源连接与目标连接不能相同".into(),
            ));
        }
        if source.driver != target.driver || source.driver != self.engine {
            return Err(DomainError::InvalidConfig(
                "数据同步只支持同引擎连接".into(),
            ));
        }
        if target.production {
            return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
        }
        source.validate().map_err(DomainError::InvalidConfig)?;
        target.validate().map_err(DomainError::InvalidConfig)?;
        self.validate_scope()
    }

    pub fn validate_scope(&self) -> Result<()> {
        match (&self.engine, &self.scope) {
            (DriverKind::Mysql | DriverKind::Postgres, DataSyncScope::Sql(scope)) => {
                validate_sql_scope(self.engine, scope)
            }
            (DriverKind::Mongodb, DataSyncScope::Mongo(scope)) => validate_mongo_scope(scope),
            _ => Err(DomainError::InvalidConfig(
                "同步范围与数据库引擎不匹配".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncObjectState {
    Missing,
    ExistingCompatible,
    ExistingIncompatible { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlannedObject {
    pub mapping: SyncObjectMapping,
    pub state: SyncObjectState,
}

/// 预检时计算，执行前重新计算并按字节比较。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncTargetFingerprint(pub [u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DataSyncStage {
    #[default]
    Preparing,
    VerifyingTarget,
    CreatingStructure,
    Scanning,
    Writing,
    Finalizing,
    Cancelling,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSyncProgress {
    pub stage: DataSyncStage,
    pub object: String,
    pub objects_done: u64,
    pub objects_total: Option<u64>,
    pub scanned: u64,
    pub inserted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub warnings: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
}

impl DataSyncProgress {
    pub fn add_scanned(&mut self, value: u64) {
        self.scanned = self.scanned.saturating_add(value);
    }

    pub fn add_inserted(&mut self, value: u64) {
        self.inserted = self.inserted.saturating_add(value);
    }

    pub fn add_skipped(&mut self, value: u64) {
        self.skipped = self.skipped.saturating_add(value);
    }

    pub fn add_failed(&mut self, value: u64) {
        self.failed = self.failed.saturating_add(value);
    }

    pub fn add_bytes(&mut self, value: u64) {
        self.bytes = self.bytes.saturating_add(value);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSyncSummary {
    pub objects: u64,
    pub scanned: u64,
    pub inserted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub warnings: Vec<String>,
    pub warnings_overflow: u64,
}

impl DataSyncSummary {
    pub fn push_warning(&mut self, warning: impl Into<String>) {
        if self.warnings.len() < MAX_TRANSFER_WARNINGS {
            self.warnings.push(warning.into());
        } else {
            self.warnings_overflow = self.warnings_overflow.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqlIdentityKind {
    PrimaryKey,
    UniqueIndex { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlRecordIdentity {
    pub kind: SqlIdentityKind,
    pub columns: Vec<String>,
}

/// 选择可重复同步所需的稳定身份。可空唯一键会产生多个 NULL，不能作为身份。
pub fn select_sql_record_identity(
    columns: &[Column],
    indexes: &[Index],
) -> Result<SqlRecordIdentity> {
    let columns_by_name: HashMap<&str, &Column> = columns
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect();
    let primary: Vec<&Index> = indexes
        .iter()
        .filter(|index| index.primary && !index.columns.is_empty())
        .collect();
    if primary.len() > 1 {
        return Err(DomainError::InvalidConfig(
            "表元数据包含多个主键，无法确定记录身份".into(),
        ));
    }
    if let Some(index) = primary.first() {
        validate_identity_columns(index, &columns_by_name)?;
        return Ok(SqlRecordIdentity {
            kind: SqlIdentityKind::PrimaryKey,
            columns: index.columns.clone(),
        });
    }

    let mut candidates: Vec<&Index> = indexes
        .iter()
        .filter(|index| {
            index.unique
                && !index.primary
                && !index.columns.is_empty()
                && index.columns.iter().all(|name| {
                    columns_by_name
                        .get(name.as_str())
                        .is_some_and(|column| !column.nullable)
                })
        })
        .collect();
    candidates.sort_by(|left, right| {
        left.columns
            .len()
            .cmp(&right.columns.len())
            .then_with(|| left.name.cmp(&right.name))
    });
    let Some(index) = candidates.first() else {
        return Err(DomainError::InvalidConfig(
            "表没有主键或全部列非空的唯一索引，不能安全重复同步".into(),
        ));
    };
    validate_identity_columns(index, &columns_by_name)?;
    Ok(SqlRecordIdentity {
        kind: SqlIdentityKind::UniqueIndex {
            name: index.name.clone(),
        },
        columns: index.columns.clone(),
    })
}

fn validate_identity_columns(index: &Index, columns: &HashMap<&str, &Column>) -> Result<()> {
    let mut seen = HashSet::with_capacity(index.columns.len());
    for name in &index.columns {
        if !columns.contains_key(name.as_str()) {
            return Err(DomainError::InvalidConfig(format!(
                "索引 {} 引用了不存在的列 {name}",
                index.name
            )));
        }
        if !seen.insert(name) {
            return Err(DomainError::InvalidConfig(format!(
                "索引 {} 重复引用列 {name}",
                index.name
            )));
        }
    }
    Ok(())
}

fn validate_sql_scope(engine: DriverKind, scope: &SqlSyncScope) -> Result<()> {
    validate_sql_identifier(engine, "源 Database / Schema", &scope.source_namespace)?;
    validate_sql_identifier(engine, "目标 Database / Schema", &scope.target_namespace)?;
    validate_object_selection(&scope.tables, |label, name| {
        validate_sql_identifier(engine, label, name)
    })
}

fn validate_mongo_scope(scope: &MongoSyncScope) -> Result<()> {
    validate_mongo_database_name(&scope.source_database)?;
    validate_mongo_database_name(&scope.target_database)?;
    validate_object_selection(&scope.collections, |_, name| {
        validate_mongo_collection_name(name)
    })
}

fn validate_object_selection(
    selection: &SyncObjectSelection,
    validate_name: impl Fn(&str, &str) -> Result<()>,
) -> Result<()> {
    let SyncObjectSelection::Selected(mappings) = selection else {
        return Ok(());
    };
    if mappings.is_empty() {
        return Err(DomainError::InvalidConfig("已选同步对象不能为空".into()));
    }
    if mappings.len() > MAX_METADATA_ITEMS {
        return Err(DomainError::InvalidConfig(format!(
            "同步对象映射超过 {MAX_METADATA_ITEMS} 个上限"
        )));
    }
    let mut sources = HashSet::with_capacity(mappings.len());
    let mut targets = HashSet::with_capacity(mappings.len());
    for mapping in mappings {
        validate_name("源对象名", &mapping.source)?;
        validate_name("目标对象名", &mapping.target)?;
        if !sources.insert(mapping.source.as_str()) {
            return Err(DomainError::InvalidConfig(format!(
                "源对象重复：{}",
                mapping.source
            )));
        }
        if !targets.insert(mapping.target.as_str()) {
            return Err(DomainError::InvalidConfig(format!(
                "多个源对象映射到同一目标对象：{}",
                mapping.target
            )));
        }
    }
    Ok(())
}

fn validate_sql_identifier(engine: DriverKind, label: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(DomainError::InvalidConfig(format!("{label}不能为空")));
    }
    if name.chars().any(char::is_control) {
        return Err(DomainError::InvalidConfig(format!(
            "{label}不能包含控制字符"
        )));
    }
    match engine {
        DriverKind::Mysql if name.chars().count() > MAX_MYSQL_SYNC_IDENTIFIER_CHARS => {
            Err(DomainError::InvalidConfig(format!(
                "{label}超过 {MAX_MYSQL_SYNC_IDENTIFIER_CHARS} 个字符上限"
            )))
        }
        DriverKind::Postgres if name.len() > MAX_POSTGRES_SYNC_IDENTIFIER_BYTES => {
            Err(DomainError::InvalidConfig(format!(
                "{label}超过 {MAX_POSTGRES_SYNC_IDENTIFIER_BYTES} bytes 上限"
            )))
        }
        DriverKind::Mysql | DriverKind::Postgres => Ok(()),
        DriverKind::Redis | DriverKind::Mongodb => Err(DomainError::InvalidConfig(
            "SQL 标识符校验收到非 SQL 引擎".into(),
        )),
    }
}

#[cfg(test)]
#[path = "data_sync_tests.rs"]
mod tests;
