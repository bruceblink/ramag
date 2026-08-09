//! 数据同步范围校验与目标状态指纹。

use ramag_domain::entities::{
    ConnectionConfig, DataSyncRequest, DriverKind, SyncTargetFingerprint,
};
use ramag_domain::error::{DomainError, Result};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

pub(super) fn protected_catalog_scope(driver: DriverKind, name: &str) -> bool {
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

pub(super) fn reject_obvious_self_sync(
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
            task_id: ramag_domain::entities::DataSyncTaskId::new(),
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
