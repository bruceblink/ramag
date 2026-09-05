//! MySQL / PostgreSQL 同步预检：展开范围、冻结结构、验证身份键与目标兼容性。

use std::collections::{BTreeMap, HashMap, HashSet};

mod ddl;
mod snapshot;
mod validation;

use ddl::{load_postgres_ddl, prepare_mysql_ddl, prepare_postgres_ddl};
pub(super) use snapshot::current_sql_target_snapshot;
use snapshot::{column_collations, generated_columns, ineligible_identity_indexes};
#[cfg(test)]
use validation::missing_source_column_error;
use validation::{validate_existing_target, validate_foreign_dependencies};

use ramag_domain::entities::{
    Column, ColumnKind, ConnectionConfig, DataSyncRequest, DriverKind, Query, SqlSyncScope,
    SyncObjectMapping, SyncObjectSelection, SyncObjectState, SyncPlannedObject, Value,
    build_ddl_query, select_sql_record_identity,
};
use ramag_domain::error::{DomainError, Result};

use super::postgres_enum::{
    PostgresRawEnum, incompatible_postgres_enum_error, load_postgres_enum_definitions,
    postgres_enum_rows, postgres_enum_statement,
};
use super::service::{
    DataSyncPreflightReport, DataSyncService, PreparedDataSync, PreparedEnginePlan,
    SqlColumnSnapshot, SqlEnumSnapshot, SqlIndexSnapshot, SqlPreparedEnum, SqlPreparedObject,
    SqlPreparedPlan, SqlTableSnapshot, SqlTargetSnapshot, fingerprint,
};
use super::sql_ddl::{rewrite_mysql_table_ddl, rewrite_postgres_statement};
use crate::usecases::transfer::sql_catalog::{
    first_column_strings, generated_columns_query, parse_pg_sequences, parse_show_create,
    pg_comments_query, pg_indexes_query, pg_sequences_query, pg_table_create_query,
    pg_table_enum_types_query, pg_table_foreign_keys_query,
};

pub(super) async fn preflight_sql(
    service: &DataSyncService,
    request: DataSyncRequest,
    source: ConnectionConfig,
    target: ConnectionConfig,
    scope: SqlSyncScope,
) -> Result<PreparedDataSync> {
    reject_protected_namespace(source.driver, &scope.target_namespace)?;
    let sql = service.connection_service();
    let (source_version, target_version, source_schemas, target_schemas) = futures::join!(
        sql.server_version(&source),
        sql.server_version(&target),
        sql.list_schemas(&source),
        sql.list_schemas(&target),
    );
    let source_version = source_version.map_err(|error| {
        DomainError::ConnectionFailed(format!("源 SQL 连接预检失败：{}", error.message()))
    })?;
    let target_version = target_version.map_err(|error| {
        DomainError::ConnectionFailed(format!("目标 SQL 连接预检失败：{}", error.message()))
    })?;
    let source_schemas = source_schemas?;
    let target_schemas = target_schemas?;
    let source_schema = source_schemas
        .iter()
        .find(|schema| schema.name == scope.source_namespace)
        .ok_or_else(|| {
            DomainError::NotFound(format!("源命名空间 {} 不存在", scope.source_namespace))
        })?;
    let namespace_exists = target_schemas
        .iter()
        .any(|schema| schema.name == scope.target_namespace);
    let source_tables = sql.list_tables(&source, &scope.source_namespace).await?;
    let mappings = expand_mappings(&scope, &source_tables)?;
    let mapping_map: HashMap<String, String> = mappings
        .iter()
        .map(|mapping| (mapping.source.clone(), mapping.target.clone()))
        .collect();
    let target_tables = if namespace_exists {
        sql.list_tables(&target, &scope.target_namespace).await?
    } else {
        Vec::new()
    };
    let target_names: HashSet<&str> = target_tables
        .iter()
        .filter(|table| !table.is_view)
        .map(|table| table.name.as_str())
        .collect();

    validate_foreign_dependencies(
        service,
        &source,
        &target,
        &scope,
        &mappings,
        &mapping_map,
        namespace_exists,
        &target_names,
    )
    .await?;

    let mut objects = Vec::with_capacity(mappings.len());
    let mut report_objects = Vec::with_capacity(mappings.len());
    let mut warnings = Vec::new();
    let mut postgres_raw = Vec::new();
    for mapping in &mappings {
        let source_columns = sql
            .list_columns(&source, &scope.source_namespace, &mapping.source)
            .await?;
        if source_columns.is_empty() {
            return Err(DomainError::InvalidConfig(format!(
                "源表 {}.{} 没有可用列元数据",
                scope.source_namespace, mapping.source
            )));
        }
        let mut source_indexes = sql
            .list_indexes(&source, &scope.source_namespace, &mapping.source)
            .await?;
        let ineligible =
            ineligible_identity_indexes(service, &source, &scope.source_namespace, &mapping.source)
                .await?;
        source_indexes.retain(|index| !ineligible.contains(&index.name));
        let identity =
            select_sql_record_identity(&source_columns, &source_indexes).map_err(|error| {
                DomainError::InvalidConfig(format!(
                    "源表 {}.{} 无法安全同步：{}",
                    scope.source_namespace,
                    mapping.source,
                    error.message()
                ))
            })?;
        if identity.columns.iter().any(|name| {
            source_columns
                .iter()
                .any(|column| column.name == *name && column.data_type.kind == ColumnKind::Float)
        }) {
            return Err(DomainError::InvalidConfig(format!(
                "源表 {}.{} 的记录身份包含浮点列；NaN 与精度语义无法保证稳定分页",
                scope.source_namespace, mapping.source
            )));
        }
        let generated = generated_columns(
            service,
            &source,
            source.driver,
            &scope.source_namespace,
            &mapping.source,
        )
        .await?;
        if identity
            .columns
            .iter()
            .any(|column| generated.contains(column))
        {
            return Err(DomainError::InvalidConfig(format!(
                "源表 {}.{} 的记录身份包含生成列，不能可靠回查缺失记录",
                scope.source_namespace, mapping.source
            )));
        }
        let writable_columns: Vec<String> = source_columns
            .iter()
            .filter(|column| !generated.contains(&column.name))
            .map(|column| column.name.clone())
            .collect();
        let source_text_columns = if source.driver == DriverKind::Postgres {
            source_columns
                .iter()
                .filter(|column| {
                    !generated.contains(&column.name) && column.data_type.kind == ColumnKind::Other
                })
                .map(|column| column.name.clone())
                .collect()
        } else {
            HashSet::new()
        };
        if writable_columns.is_empty() {
            return Err(DomainError::InvalidConfig(format!(
                "源表 {}.{} 全部为生成列，没有可同步数据",
                scope.source_namespace, mapping.source
            )));
        }
        let target_exists = target_names.contains(mapping.target.as_str());
        if target_tables
            .iter()
            .any(|table| table.name == mapping.target && table.is_view)
        {
            return Err(DomainError::InvalidConfig(format!(
                "目标 {}.{} 是视图，不能作为数据同步目标",
                scope.target_namespace, mapping.target
            )));
        }
        if target_exists {
            validate_existing_target(
                service,
                &source,
                &target,
                &scope.source_namespace,
                &scope.target_namespace,
                mapping,
                &source_columns,
                &generated,
                &identity.columns,
            )
            .await?;
        }

        let mut prepared = SqlPreparedObject {
            mapping: mapping.clone(),
            identity,
            writable_columns,
            source_text_columns,
            target_exists,
            create_statement: None,
            post_create_statements: Vec::new(),
            final_statements: Vec::new(),
            has_identity_always: false,
        };
        match source.driver {
            DriverKind::Mysql if !target_exists => {
                prepare_mysql_ddl(service, &source, &scope, &mapping_map, &mut prepared).await?
            }
            DriverKind::Postgres => {
                postgres_raw.push(load_postgres_ddl(service, &source, &scope, mapping).await?);
            }
            DriverKind::Mysql => {}
            DriverKind::Sqlite | DriverKind::Redis | DriverKind::Mongodb => {
                return Err(DomainError::InvalidConfig("SQLite 暂不支持数据同步".into()));
            }
        }
        report_objects.push(SyncPlannedObject {
            mapping: mapping.clone(),
            state: if target_exists {
                SyncObjectState::ExistingCompatible
            } else {
                SyncObjectState::Missing
            },
        });
        objects.push(prepared);
    }

    let mut pre_create_statements = Vec::new();
    let postgres_enums = if source.driver == DriverKind::Postgres {
        prepare_postgres_ddl(
            service,
            &target,
            &scope,
            &mapping_map,
            namespace_exists,
            &postgres_raw,
            &mut objects,
            &mut pre_create_statements,
        )
        .await?
    } else {
        Vec::new()
    };
    let postgres_enum_names: Vec<_> = postgres_enums
        .iter()
        .map(|item| item.name.clone())
        .collect();
    if mappings.is_empty() {
        warnings.push("源命名空间当前没有普通表".to_string());
    }
    let target_snapshot =
        current_sql_target_snapshot(service, &target, &scope, &mappings, &postgres_enum_names)
            .await?;
    let namespace_create = (!namespace_exists)
        .then(|| namespace_create_statement(source.driver, source_schema, &scope.target_namespace));
    let report = DataSyncPreflightReport {
        task_id: request.task_id.clone(),
        engine: source.driver,
        source_connection: source.name.clone(),
        source_scope: scope.source_namespace.clone(),
        source_version,
        target_connection: target.name.clone(),
        target_scope: scope.target_namespace.clone(),
        target_version,
        objects: report_objects,
        objects_total: Some(mappings.len() as u64),
        target_scope_exists: namespace_exists,
        requires_second_confirmation: namespace_exists
            || objects.iter().any(|object| object.target_exists),
        target_fingerprint: fingerprint(&target_snapshot)?,
        warnings,
        warnings_overflow: 0,
    };
    Ok(PreparedDataSync {
        request,
        source,
        target,
        report,
        engine_plan: PreparedEnginePlan::Sql(SqlPreparedPlan {
            scope,
            namespace_exists,
            namespace_create,
            pre_create_statements,
            postgres_enums,
            objects,
            target_snapshot,
        }),
    })
}

fn expand_mappings(
    scope: &SqlSyncScope,
    source_tables: &[ramag_domain::entities::Table],
) -> Result<Vec<SyncObjectMapping>> {
    match &scope.tables {
        SyncObjectSelection::All => {
            let mut mappings: Vec<_> = source_tables
                .iter()
                .filter(|table| !table.is_view)
                .map(|table| SyncObjectMapping {
                    source: table.name.clone(),
                    target: table.name.clone(),
                })
                .collect();
            mappings.sort_by(|left, right| left.source.cmp(&right.source));
            Ok(mappings)
        }
        SyncObjectSelection::Selected(mappings) => {
            for mapping in mappings {
                let table = source_tables
                    .iter()
                    .find(|table| table.name == mapping.source)
                    .ok_or_else(|| {
                        DomainError::NotFound(format!(
                            "源表 {}.{} 不存在",
                            scope.source_namespace, mapping.source
                        ))
                    })?;
                if table.is_view {
                    return Err(DomainError::InvalidConfig(format!(
                        "源对象 {}.{} 是视图，数据同步不支持视图",
                        scope.source_namespace, mapping.source
                    )));
                }
            }
            Ok(mappings.clone())
        }
    }
}

fn namespace_create_statement(
    driver: DriverKind,
    source_schema: &ramag_domain::entities::Schema,
    target_namespace: &str,
) -> String {
    match driver {
        DriverKind::Mysql => {
            let mut statement = format!(
                "CREATE DATABASE {}",
                DriverKind::Mysql.quote_identifier(target_namespace)
            );
            if let Some(charset) = source_schema
                .charset
                .as_deref()
                .filter(|value| safe_option(value))
            {
                statement.push_str(&format!(" DEFAULT CHARACTER SET {charset}"));
            }
            if let Some(collation) = source_schema
                .collation
                .as_deref()
                .filter(|value| safe_option(value))
            {
                statement.push_str(&format!(" COLLATE {collation}"));
            }
            statement.push(';');
            statement
        }
        DriverKind::Postgres => format!(
            "CREATE SCHEMA {};",
            DriverKind::Postgres.quote_identifier(target_namespace)
        ),
        DriverKind::Sqlite | DriverKind::Redis | DriverKind::Mongodb => unreachable!(),
    }
}

fn safe_option(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn normalize_type(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn normalize_source_type(value: &str, source_namespace: &str, target_namespace: &str) -> String {
    let quoted_source = format!("\"{}\".", source_namespace.replace('"', "\"\""));
    let quoted_target = format!("\"{}\".", target_namespace.replace('"', "\"\""));
    let mapped = value.replace(&quoted_source, &quoted_target);
    let plain_source = format!("{source_namespace}.");
    let plain_target = format!("{target_namespace}.");
    let mapped = mapped
        .strip_prefix(&plain_source)
        .map_or(mapped.clone(), |suffix| format!("{plain_target}{suffix}"));
    normalize_type(&mapped)
}

fn normalize_statement(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn second_quoted_identifier(statement: &str, schema: &str) -> Option<String> {
    let prefix = format!("\"{}\".", schema.replace('"', "\"\""));
    let start = statement.find(&prefix)? + prefix.len();
    let rest = statement.get(start..)?;
    let body = rest.strip_prefix('"')?;
    let mut name = String::new();
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            if chars.peek() == Some(&'"') {
                chars.next();
                name.push('"');
            } else {
                return Some(name);
            }
        } else {
            name.push(ch);
        }
    }
    None
}

fn sequence_identifier(statement: &str, schema: &str) -> Option<String> {
    let upper = statement.to_ascii_uppercase();
    let sequence_statement = upper
        .find("SETVAL(")
        .and_then(|offset| statement.get(offset..))
        .unwrap_or(statement);
    second_quoted_identifier(sequence_statement, schema)
}

fn reject_protected_namespace(driver: DriverKind, namespace: &str) -> Result<()> {
    let normalized = namespace.to_ascii_lowercase();
    let protected = match driver {
        DriverKind::Mysql => matches!(
            normalized.as_str(),
            "information_schema" | "mysql" | "performance_schema" | "sys"
        ),
        DriverKind::Postgres => {
            matches!(normalized.as_str(), "information_schema" | "pg_catalog")
                || normalized.starts_with("pg_toast")
                || normalized.starts_with("pg_temp")
        }
        DriverKind::Sqlite | DriverKind::Redis | DriverKind::Mongodb => false,
    };
    if protected {
        return Err(DomainError::Forbidden(format!(
            "目标命名空间 {namespace} 是系统范围，禁止数据同步"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_normalization_is_conservative_and_case_insensitive() {
        assert_eq!(
            normalize_type("VARCHAR(32)   CHARACTER SET utf8mb4"),
            "varchar(32) character set utf8mb4"
        );
        assert_ne!(normalize_type("varchar(32)"), normalize_type("varchar(64)"));
        assert_eq!(
            normalize_source_type("\"source\".status[]", "source", "target"),
            "\"target\".status[]"
        );
        assert_eq!(
            normalize_source_type("source.status", "source", "target"),
            "target.status"
        );
    }

    #[test]
    fn extracts_quoted_sequence_name() {
        assert_eq!(
            sequence_identifier(
                "CREATE SEQUENCE IF NOT EXISTS \"old\".\"orders_id_seq\";",
                "old"
            ),
            Some("orders_id_seq".into())
        );
        assert_eq!(
            sequence_identifier(
                "SELECT CASE WHEN (SELECT MAX(id) FROM \"old\".\"orders\") > 0 THEN setval('\"old\".\"orders_id_seq\"', 1, true) END;",
                "old"
            ),
            Some("orders_id_seq".into())
        );
    }

    #[test]
    fn unsafe_mysql_charset_option_is_not_accepted() {
        assert!(safe_option("utf8mb4_0900_ai_ci"));
        assert!(!safe_option("utf8mb4; DROP DATABASE x"));
    }

    #[test]
    fn system_namespaces_are_rejected() {
        assert!(reject_protected_namespace(DriverKind::Mysql, "mysql").is_err());
        assert!(reject_protected_namespace(DriverKind::Postgres, "pg_catalog").is_err());
        assert!(reject_protected_namespace(DriverKind::Postgres, "public").is_ok());
    }

    #[test]
    fn missing_source_column_error_explains_safe_options() {
        let message = missing_source_column_error("app", "plan_usage", "project_id")
            .message()
            .to_string();
        assert!(message.contains("改用新目标表名"));
        assert!(message.contains("取消选择该表"));
        assert!(message.contains("补齐目标表结构"));
    }
}
