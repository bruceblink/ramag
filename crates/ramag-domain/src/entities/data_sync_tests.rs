#![allow(clippy::expect_used)]

use super::*;
use crate::entities::{ColumnKind, ColumnType, MAX_MONGO_COLLECTION_NAME_BYTES};

fn column(name: &str, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type: ColumnType {
            kind: ColumnKind::Integer,
            raw_type: "integer".into(),
        },
        nullable,
        default_value: None,
        is_primary_key: false,
        comment: None,
        ordinal_position: None,
        is_auto_increment: false,
        generation_expression: None,
        generated_storage: None,
        identity_generation: None,
    }
}

fn index(name: &str, columns: &[&str], primary: bool, unique: bool) -> Index {
    Index {
        name: name.into(),
        unique,
        primary,
        columns: columns.iter().map(|name| (*name).into()).collect(),
    }
}

fn request(engine: DriverKind, scope: DataSyncScope) -> DataSyncRequest {
    DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: ConnectionId::new(),
        target_connection_id: ConnectionId::new(),
        engine,
        scope,
    }
}

fn configs(request: &DataSyncRequest) -> (ConnectionConfig, ConnectionConfig) {
    let mut source = match request.engine {
        DriverKind::Mysql => ConnectionConfig::new_mysql("source", "127.0.0.1", 3306, "root"),
        DriverKind::Postgres => {
            let mut config = ConnectionConfig::new_mysql("source", "127.0.0.1", 5432, "root");
            config.driver = DriverKind::Postgres;
            config
        }
        DriverKind::Redis => ConnectionConfig::new_redis("source", "127.0.0.1", 6379),
        DriverKind::Mongodb => ConnectionConfig::new_mongodb("source", "127.0.0.1", 27017),
    };
    source.id = request.source_connection_id.clone();
    let mut target = source.clone();
    target.id = request.target_connection_id.clone();
    target.name = "target".into();
    (source, target)
}

#[test]
fn request_rejects_cross_engine_same_connection_and_read_only_target() {
    let request = request(
        DriverKind::Mysql,
        DataSyncScope::Sql(SqlSyncScope {
            source_namespace: "source_db".into(),
            target_namespace: "target_db".into(),
            tables: SyncObjectSelection::All,
        }),
    );
    let (source, mut target) = configs(&request);
    assert!(request.validate_connections(&source, &target).is_ok());

    target.driver = DriverKind::Postgres;
    assert!(request.validate_connections(&source, &target).is_err());
    target.driver = DriverKind::Mysql;
    target.id = source.id.clone();
    assert!(request.validate_connections(&source, &target).is_err());
    target.id = request.target_connection_id.clone();
    target.production = true;
    let error = request
        .validate_connections(&source, &target)
        .expect_err("只读目标必须拒绝同步");
    assert!(matches!(error, DomainError::Forbidden(_)));
}

#[test]
fn scope_must_match_engine() {
    let request = request(
        DriverKind::Mysql,
        DataSyncScope::Mongo(MongoSyncScope {
            source_database: "source".into(),
            target_database: "target".into(),
            collections: SyncObjectSelection::All,
        }),
    );
    assert!(request.validate_scope().is_err());
}

#[test]
fn redis_engine_is_not_supported_for_data_sync() {
    let request = request(
        DriverKind::Redis,
        DataSyncScope::Mongo(MongoSyncScope {
            source_database: "source".into(),
            target_database: "target".into(),
            collections: SyncObjectSelection::All,
        }),
    );
    assert!(request.validate_scope().is_err());
}

#[test]
fn object_mapping_is_non_empty_and_injective() {
    let duplicate_target = request(
        DriverKind::Postgres,
        DataSyncScope::Sql(SqlSyncScope {
            source_namespace: "public".into(),
            target_namespace: "archive".into(),
            tables: SyncObjectSelection::Selected(vec![
                SyncObjectMapping {
                    source: "users".into(),
                    target: "records".into(),
                },
                SyncObjectMapping {
                    source: "orders".into(),
                    target: "records".into(),
                },
            ]),
        }),
    );
    assert!(duplicate_target.validate_scope().is_err());

    let empty = request(
        DriverKind::Mongodb,
        DataSyncScope::Mongo(MongoSyncScope {
            source_database: "source".into(),
            target_database: "target".into(),
            collections: SyncObjectSelection::Selected(Vec::new()),
        }),
    );
    assert!(empty.validate_scope().is_err());
}

#[test]
fn sql_identifier_boundaries_follow_each_engine() {
    let mysql = request(
        DriverKind::Mysql,
        DataSyncScope::Sql(SqlSyncScope {
            source_namespace: "数".repeat(MAX_MYSQL_SYNC_IDENTIFIER_CHARS),
            target_namespace: "target".into(),
            tables: SyncObjectSelection::All,
        }),
    );
    assert!(mysql.validate_scope().is_ok());
    let mut oversized = mysql;
    let DataSyncScope::Sql(scope) = &mut oversized.scope else {
        return;
    };
    scope.source_namespace.push('数');
    assert!(oversized.validate_scope().is_err());

    let postgres = request(
        DriverKind::Postgres,
        DataSyncScope::Sql(SqlSyncScope {
            source_namespace: "s".repeat(MAX_POSTGRES_SYNC_IDENTIFIER_BYTES),
            target_namespace: "target".into(),
            tables: SyncObjectSelection::All,
        }),
    );
    assert!(postgres.validate_scope().is_ok());
    let mut oversized = postgres;
    let DataSyncScope::Sql(scope) = &mut oversized.scope else {
        return;
    };
    scope.source_namespace.push('s');
    assert!(oversized.validate_scope().is_err());
}

#[test]
fn mongo_name_boundaries_are_checked() {
    let mongo = request(
        DriverKind::Mongodb,
        DataSyncScope::Mongo(MongoSyncScope {
            source_database: "source".into(),
            target_database: "target".into(),
            collections: SyncObjectSelection::Selected(vec![SyncObjectMapping {
                source: "s".repeat(MAX_MONGO_COLLECTION_NAME_BYTES + 1),
                target: "target".into(),
            }]),
        }),
    );
    assert!(mongo.validate_scope().is_err());
}

#[test]
fn primary_key_wins_and_preserves_composite_order() {
    let columns = vec![column("tenant_id", false), column("id", false)];
    let indexes = vec![
        index("uq_id", &["id"], false, true),
        index("PRIMARY", &["tenant_id", "id"], true, true),
    ];
    let identity = select_sql_record_identity(&columns, &indexes).expect("复合主键应成为同步身份");
    assert_eq!(identity.kind, SqlIdentityKind::PrimaryKey);
    assert_eq!(identity.columns, vec!["tenant_id", "id"]);
}

#[test]
fn deterministic_non_nullable_unique_index_is_fallback() {
    let columns = vec![
        column("email", false),
        column("tenant", false),
        column("nullable_code", true),
    ];
    let indexes = vec![
        index("z_two", &["tenant", "email"], false, true),
        index("a_one", &["email"], false, true),
        index("nullable", &["nullable_code"], false, true),
    ];
    let identity =
        select_sql_record_identity(&columns, &indexes).expect("非空唯一键应成为兜底身份");
    assert_eq!(
        identity.kind,
        SqlIdentityKind::UniqueIndex {
            name: "a_one".into()
        }
    );
    assert_eq!(identity.columns, vec!["email"]);
}

#[test]
fn nullable_or_missing_unique_identity_is_rejected() {
    let columns = vec![column("code", true)];
    assert!(
        select_sql_record_identity(&columns, &[index("uq_code", &["code"], false, true)]).is_err()
    );
    assert!(select_sql_record_identity(&columns, &[]).is_err());
    assert!(
        select_sql_record_identity(&columns, &[index("PRIMARY", &["missing"], true, true)])
            .is_err()
    );
}

#[test]
fn progress_and_warning_counts_saturate_and_remain_bounded() {
    let mut progress = DataSyncProgress {
        scanned: u64::MAX,
        inserted: u64::MAX,
        skipped: u64::MAX,
        failed: u64::MAX,
        bytes: u64::MAX,
        ..DataSyncProgress::default()
    };
    progress.add_scanned(1);
    progress.add_inserted(1);
    progress.add_skipped(1);
    progress.add_failed(1);
    progress.add_bytes(1);
    assert_eq!(progress.scanned, u64::MAX);
    assert_eq!(progress.inserted, u64::MAX);
    assert_eq!(progress.skipped, u64::MAX);
    assert_eq!(progress.failed, u64::MAX);
    assert_eq!(progress.bytes, u64::MAX);

    let mut summary = DataSyncSummary::default();
    for index in 0..(MAX_TRANSFER_WARNINGS + 3) {
        summary.push_warning(format!("warning-{index}"));
    }
    assert_eq!(summary.warnings.len(), MAX_TRANSFER_WARNINGS);
    assert_eq!(summary.warnings_overflow, 3);
}

#[test]
fn public_request_serialization_does_not_contain_credentials() {
    let request = request(
        DriverKind::Mongodb,
        DataSyncScope::Mongo(MongoSyncScope {
            source_database: "source".into(),
            target_database: "target".into(),
            collections: SyncObjectSelection::All,
        }),
    );
    let json = serde_json::to_string(&request).expect("请求应可序列化");
    assert!(!json.contains("password"));
    assert!(!json.contains("host"));
    assert!(!json.contains("username"));
}
