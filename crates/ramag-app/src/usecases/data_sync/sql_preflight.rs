//! MySQL / PostgreSQL 同步预检：展开范围、冻结结构、验证身份键与目标兼容性。

use std::collections::{BTreeMap, HashMap, HashSet};

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
            DriverKind::Redis | DriverKind::Mongodb => unreachable!(),
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

#[allow(clippy::too_many_arguments)]
async fn validate_foreign_dependencies(
    service: &DataSyncService,
    source: &ConnectionConfig,
    target: &ConnectionConfig,
    scope: &SqlSyncScope,
    mappings: &[SyncObjectMapping],
    mapping_map: &HashMap<String, String>,
    target_namespace_exists: bool,
    target_names: &HashSet<&str>,
) -> Result<()> {
    for mapping in mappings {
        let foreign_keys = service
            .connection_service()
            .list_foreign_keys(source, &scope.source_namespace, &mapping.source)
            .await?;
        for foreign_key in foreign_keys {
            let mapped_by_task = foreign_key.ref_schema == scope.source_namespace
                && mapping_map.contains_key(&foreign_key.ref_table);
            if mapped_by_task {
                continue;
            }
            let target_schema = if foreign_key.ref_schema == scope.source_namespace {
                &scope.target_namespace
            } else {
                &foreign_key.ref_schema
            };
            let exists = if target_schema == &scope.target_namespace {
                target_namespace_exists && target_names.contains(foreign_key.ref_table.as_str())
            } else {
                service
                    .connection_service()
                    .list_tables(target, target_schema)
                    .await?
                    .iter()
                    .any(|table| !table.is_view && table.name == foreign_key.ref_table)
            };
            if !exists {
                return Err(DomainError::InvalidConfig(format!(
                    "源表 {}.{} 的外键 {} 依赖 {}.{}，该表既未选择同步也不存在于目标",
                    scope.source_namespace,
                    mapping.source,
                    foreign_key.name,
                    target_schema,
                    foreign_key.ref_table
                )));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn validate_existing_target(
    service: &DataSyncService,
    source: &ConnectionConfig,
    target: &ConnectionConfig,
    source_namespace: &str,
    target_namespace: &str,
    mapping: &SyncObjectMapping,
    source_columns: &[Column],
    source_generated: &HashSet<String>,
    identity_columns: &[String],
) -> Result<()> {
    let sql = service.connection_service();
    let (
        target_columns,
        target_indexes,
        target_generated,
        source_collations,
        target_collations,
        source_identity_modes,
        target_identity_modes,
    ) = futures::join!(
        sql.list_columns(target, target_namespace, &mapping.target),
        sql.list_indexes(target, target_namespace, &mapping.target),
        generated_columns(
            service,
            target,
            target.driver,
            target_namespace,
            &mapping.target,
        ),
        column_collations(service, source, source_namespace, &mapping.source),
        column_collations(service, target, target_namespace, &mapping.target),
        postgres_identity_modes(service, source, source_namespace, &mapping.source),
        postgres_identity_modes(service, target, target_namespace, &mapping.target),
    );
    let target_columns = target_columns?;
    let mut target_indexes = target_indexes?;
    let target_generated = target_generated?;
    let source_collations = source_collations?;
    let target_collations = target_collations?;
    let source_identity_modes = source_identity_modes?;
    let target_identity_modes = target_identity_modes?;
    let ineligible =
        ineligible_identity_indexes(service, target, target_namespace, &mapping.target).await?;
    target_indexes.retain(|index| !ineligible.contains(&index.name));
    let by_name: HashMap<&str, &Column> = target_columns
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect();
    let source_names: HashSet<&str> = source_columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    for source_column in source_columns {
        let target_column = by_name.get(source_column.name.as_str()).ok_or_else(|| {
            missing_source_column_error(target_namespace, &mapping.target, &source_column.name)
        })?;
        if normalize_source_type(
            &source_column.data_type.raw_type,
            source_namespace,
            target_namespace,
        ) != normalize_type(&target_column.data_type.raw_type)
        {
            return Err(DomainError::InvalidConfig(format!(
                "目标表 {}.{} 的列 {} 类型不兼容：源为 {}，目标为 {}",
                target_namespace,
                mapping.target,
                source_column.name,
                source_column.data_type.raw_type,
                target_column.data_type.raw_type
            )));
        }
        if source_column.nullable && !target_column.nullable {
            return Err(DomainError::InvalidConfig(format!(
                "目标表 {}.{} 的列 {} 不允许 NULL，但源列允许 NULL",
                target_namespace, mapping.target, source_column.name
            )));
        }
        if source_generated.contains(&source_column.name)
            != target_generated.contains(&source_column.name)
        {
            return Err(DomainError::InvalidConfig(format!(
                "目标表 {}.{} 的列 {} 生成列属性与源不一致",
                target_namespace, mapping.target, source_column.name
            )));
        }
        if source_collations.get(&source_column.name) != target_collations.get(&source_column.name)
        {
            return Err(DomainError::InvalidConfig(format!(
                "目标表 {}.{} 的列 {} 排序规则与源不一致，可能破坏身份判重或文本约束",
                target_namespace, mapping.target, source_column.name
            )));
        }
        if source_identity_modes.get(&source_column.name)
            != target_identity_modes.get(&source_column.name)
        {
            return Err(DomainError::InvalidConfig(format!(
                "目标表 {}.{} 的列 {} Identity 模式与源不一致",
                target_namespace, mapping.target, source_column.name
            )));
        }
    }
    for target_column in &target_columns {
        if !source_names.contains(target_column.name.as_str())
            && !target_column.nullable
            && target_column.default_value.is_none()
            && !target_generated.contains(&target_column.name)
        {
            return Err(DomainError::InvalidConfig(format!(
                "目标表 {}.{} 的额外列 {} 非空且无默认值，无法插入源数据",
                target_namespace, mapping.target, target_column.name
            )));
        }
    }
    let has_identity = target_indexes.iter().any(|index| {
        (index.unique || index.primary)
            && index.columns == identity_columns
            && !index.columns.is_empty()
    });
    if !has_identity {
        return Err(DomainError::InvalidConfig(format!(
            "目标表 {}.{} 缺少与源记录身份一致的唯一索引 ({})",
            target_namespace,
            mapping.target,
            identity_columns.join(", ")
        )));
    }
    Ok(())
}

fn missing_source_column_error(namespace: &str, table: &str, column: &str) -> DomainError {
    DomainError::InvalidConfig(format!(
        "目标表 {namespace}.{table} 缺少源列 {column}。请改用新目标表名、取消选择该表，或先补齐目标表结构"
    ))
}

async fn postgres_identity_modes(
    service: &DataSyncService,
    config: &ConnectionConfig,
    namespace: &str,
    table: &str,
) -> Result<HashMap<String, String>> {
    if config.driver != DriverKind::Postgres {
        return Ok(HashMap::new());
    }
    let namespace = namespace.replace('\'', "''");
    let table = table.replace('\'', "''");
    let result = service
        .connection_service()
        .execute(
            config,
            &Query::new(format!(
                "SELECT a.attname::text, a.attidentity::text FROM pg_attribute a \
                 JOIN pg_class t ON t.oid=a.attrelid \
                 JOIN pg_namespace n ON n.oid=t.relnamespace \
                 WHERE n.nspname='{namespace}' AND t.relname='{table}' \
                   AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum;"
            )),
        )
        .await?;
    let mut modes = HashMap::with_capacity(result.rows.len());
    for row in result.rows {
        let (Some(Value::Text(name)), Some(Value::Text(mode))) =
            (row.values.first(), row.values.get(1))
        else {
            return Err(DomainError::QueryFailed(
                "PostgreSQL Identity 元数据结果类型异常".into(),
            ));
        };
        modes.insert(name.clone(), mode.clone());
    }
    Ok(modes)
}

async fn prepare_mysql_ddl(
    service: &DataSyncService,
    source: &ConnectionConfig,
    scope: &SqlSyncScope,
    mappings: &HashMap<String, String>,
    prepared: &mut SqlPreparedObject,
) -> Result<()> {
    let query = build_ddl_query(
        DriverKind::Mysql,
        &scope.source_namespace,
        &prepared.mapping.source,
        false,
    );
    let result = service
        .connection_service()
        .execute(source, &Query::new(query))
        .await?;
    let ddl = rewrite_mysql_table_ddl(
        &parse_show_create(&result)?,
        &scope.source_namespace,
        &scope.target_namespace,
        &prepared.mapping.source,
        &prepared.mapping.target,
        mappings,
    )?;
    prepared.create_statement = Some(ddl.create_statement);
    prepared.final_statements = ddl.foreign_key_statements;
    Ok(())
}

struct PostgresRawDdl {
    table: String,
    create: String,
    enums: Vec<PostgresRawEnum>,
    sequences: crate::usecases::transfer::sql_catalog::PgSequenceInfo,
    comments: Vec<String>,
    indexes: Vec<String>,
    foreign_keys: Vec<String>,
}

async fn load_postgres_ddl(
    service: &DataSyncService,
    source: &ConnectionConfig,
    scope: &SqlSyncScope,
    mapping: &SyncObjectMapping,
) -> Result<PostgresRawDdl> {
    let sql = service.connection_service();
    let create_query = Query::new(pg_table_create_query(
        &scope.source_namespace,
        &mapping.source,
    ));
    let enum_query = Query::new(pg_table_enum_types_query(
        &scope.source_namespace,
        &mapping.source,
    ));
    let sequence_query = Query::new(pg_sequences_query(&scope.source_namespace, &mapping.source));
    let comment_query = Query::new(pg_comments_query(&scope.source_namespace, &mapping.source));
    let index_query = Query::new(pg_indexes_query(&scope.source_namespace, &mapping.source));
    let foreign_key_query = Query::new(pg_table_foreign_keys_query(
        &scope.source_namespace,
        &mapping.source,
    ));
    let (create, enums, sequences, comments, indexes, foreign_keys) = futures::join!(
        sql.execute(source, &create_query),
        sql.execute(source, &enum_query),
        sql.execute(source, &sequence_query),
        sql.execute(source, &comment_query),
        sql.execute(source, &index_query),
        sql.execute(source, &foreign_key_query),
    );
    let create = first_column_strings(&create?)
        .into_iter()
        .next()
        .ok_or_else(|| DomainError::QueryFailed(format!("表 {} DDL 为空", mapping.source)))?;
    Ok(PostgresRawDdl {
        table: mapping.source.clone(),
        create,
        enums: postgres_enum_rows(&enums?)?,
        sequences: parse_pg_sequences(&sequences?),
        comments: first_column_strings(&comments?),
        indexes: first_column_strings(&indexes?),
        foreign_keys: first_column_strings(&foreign_keys?),
    })
}

#[allow(clippy::too_many_arguments)]
async fn prepare_postgres_ddl(
    service: &DataSyncService,
    target: &ConnectionConfig,
    scope: &SqlSyncScope,
    mappings: &HashMap<String, String>,
    namespace_exists: bool,
    raw: &[PostgresRawDdl],
    objects: &mut [SqlPreparedObject],
    pre_create: &mut Vec<String>,
) -> Result<Vec<SqlPreparedEnum>> {
    let mut ddl_mappings = mappings.clone();
    for table in raw {
        let target_table = mappings
            .get(&table.table)
            .map_or(table.table.as_str(), String::as_str);
        for statement in table
            .sequences
            .create_stmts
            .iter()
            .chain(table.sequences.setval_stmts.iter())
        {
            if let Some(sequence) = sequence_identifier(statement, &scope.source_namespace) {
                let mapped = sequence
                    .strip_prefix(&table.table)
                    .map_or(sequence.clone(), |suffix| format!("{target_table}{suffix}"));
                ddl_mappings.insert(sequence, mapped);
            }
        }
    }
    let target_enums = if namespace_exists {
        load_postgres_enum_definitions(service, target, &scope.target_namespace).await?
    } else {
        BTreeMap::new()
    };
    let mut source_enums = BTreeMap::<String, SqlPreparedEnum>::new();
    for table in raw {
        for raw_enum in &table.enums {
            let mapped = rewrite_postgres_statement(
                &raw_enum.create_statement,
                &scope.source_namespace,
                &scope.target_namespace,
                &ddl_mappings,
            )?;
            let statement = postgres_enum_statement(&mapped)?
                .ok_or_else(|| DomainError::QueryFailed("无法解析 PostgreSQL ENUM 定义".into()))?;
            let definition = SqlPreparedEnum {
                name: statement.name,
                signature: raw_enum.signature.clone(),
                create_statement: statement.create_statement,
            };
            if let Some(planned) = source_enums.get(&definition.name) {
                if planned.signature != definition.signature {
                    return Err(DomainError::InvalidConfig(format!(
                        "源枚举类型 {}.{} 的元数据定义不一致，已停止同步",
                        scope.source_namespace, definition.name
                    )));
                }
            } else {
                source_enums.insert(definition.name.clone(), definition);
            }
        }
    }
    for definition in source_enums.values() {
        if let Some(existing) = target_enums.get(&definition.name)
            && existing != &definition.signature
        {
            return Err(incompatible_postgres_enum_error(
                &scope.target_namespace,
                &definition.name,
            ));
        }
    }
    let mut planned_sequences = HashSet::new();
    for table in raw {
        let Some(object) = objects
            .iter_mut()
            .find(|object| object.mapping.source == table.table)
        else {
            return Err(DomainError::Other(
                "PostgreSQL DDL 计划与表映射不一致".into(),
            ));
        };
        object.has_identity_always = table.sequences.has_identity_always;
        if object.target_exists {
            let target_sequences = load_postgres_sequences(
                service,
                target,
                &scope.target_namespace,
                &object.mapping.target,
            )
            .await?;
            if target_sequences.setval_stmts.len() != table.sequences.setval_stmts.len() {
                return Err(DomainError::InvalidConfig(format!(
                    "目标表 {}.{} 的序列定义与源表不兼容",
                    scope.target_namespace, object.mapping.target
                )));
            }
            // 已有目标可能使用自定义序列名；必须推进目标自己的序列，不能猜测映射名。
            object
                .final_statements
                .extend(target_sequences.setval_stmts);
            continue;
        }
        for statement in &table.sequences.create_stmts {
            let mapped = rewrite_postgres_statement(
                statement,
                &scope.source_namespace,
                &scope.target_namespace,
                &ddl_mappings,
            )?;
            if planned_sequences.insert(normalize_statement(&mapped)) {
                pre_create.push(mapped);
            }
        }
        object.create_statement = Some(rewrite_postgres_statement(
            &table.create,
            &scope.source_namespace,
            &scope.target_namespace,
            &ddl_mappings,
        )?);
        for statement in table
            .sequences
            .owned_stmts
            .iter()
            .chain(table.comments.iter())
        {
            object
                .post_create_statements
                .push(rewrite_postgres_statement(
                    statement,
                    &scope.source_namespace,
                    &scope.target_namespace,
                    &ddl_mappings,
                )?);
        }
        for statement in table
            .indexes
            .iter()
            .chain(table.sequences.setval_stmts.iter())
            .chain(table.foreign_keys.iter())
        {
            object.final_statements.push(rewrite_postgres_statement(
                statement,
                &scope.source_namespace,
                &scope.target_namespace,
                &ddl_mappings,
            )?);
        }
    }
    Ok(source_enums.into_values().collect())
}

async fn load_postgres_sequences(
    service: &DataSyncService,
    config: &ConnectionConfig,
    namespace: &str,
    table: &str,
) -> Result<crate::usecases::transfer::sql_catalog::PgSequenceInfo> {
    let result = service
        .connection_service()
        .execute(config, &Query::new(pg_sequences_query(namespace, table)))
        .await?;
    Ok(parse_pg_sequences(&result))
}

pub(super) async fn current_sql_target_snapshot(
    service: &DataSyncService,
    target: &ConnectionConfig,
    scope: &SqlSyncScope,
    mappings: &[SyncObjectMapping],
    postgres_enum_names: &[String],
) -> Result<SqlTargetSnapshot> {
    let sql = service.connection_service();
    let namespace_exists = sql
        .list_schemas(target)
        .await?
        .iter()
        .any(|schema| schema.name == scope.target_namespace);
    if !namespace_exists {
        return Ok(SqlTargetSnapshot {
            namespace_exists: false,
            enum_types: postgres_enum_names
                .iter()
                .map(|name| SqlEnumSnapshot {
                    name: name.clone(),
                    signature: None,
                })
                .collect(),
            tables: mappings
                .iter()
                .map(|mapping| SqlTableSnapshot {
                    name: mapping.target.clone(),
                    exists: false,
                    columns: Vec::new(),
                    indexes: Vec::new(),
                })
                .collect(),
        });
    }
    let enum_types = if target.driver == DriverKind::Postgres {
        let definitions =
            load_postgres_enum_definitions(service, target, &scope.target_namespace).await?;
        postgres_enum_names
            .iter()
            .map(|name| SqlEnumSnapshot {
                name: name.clone(),
                signature: definitions.get(name).cloned(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let tables = sql.list_tables(target, &scope.target_namespace).await?;
    let mut snapshots = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        let exists = tables
            .iter()
            .any(|table| !table.is_view && table.name == mapping.target);
        if !exists {
            snapshots.push(SqlTableSnapshot {
                name: mapping.target.clone(),
                exists: false,
                columns: Vec::new(),
                indexes: Vec::new(),
            });
            continue;
        }
        let (columns, indexes, generated, collations) = futures::join!(
            sql.list_columns(target, &scope.target_namespace, &mapping.target),
            sql.list_indexes(target, &scope.target_namespace, &mapping.target),
            generated_columns(
                service,
                target,
                target.driver,
                &scope.target_namespace,
                &mapping.target,
            ),
            column_collations(service, target, &scope.target_namespace, &mapping.target,),
        );
        let generated = generated?;
        let collations = collations?;
        let mut columns: Vec<_> = columns?
            .into_iter()
            .map(|column| SqlColumnSnapshot {
                generated: generated.contains(&column.name),
                collation: collations.get(&column.name).cloned().flatten(),
                name: column.name,
                raw_type: normalize_type(&column.data_type.raw_type),
                nullable: column.nullable,
                default_value: column.default_value,
            })
            .collect();
        columns.sort_by(|left, right| left.name.cmp(&right.name));
        let mut indexes: Vec<_> = indexes?
            .into_iter()
            .map(|index| SqlIndexSnapshot {
                name: index.name,
                unique: index.unique,
                primary: index.primary,
                columns: index.columns,
            })
            .collect();
        indexes.sort_by(|left, right| left.name.cmp(&right.name));
        snapshots.push(SqlTableSnapshot {
            name: mapping.target.clone(),
            exists: true,
            columns,
            indexes,
        });
    }
    snapshots.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(SqlTargetSnapshot {
        namespace_exists,
        enum_types,
        tables: snapshots,
    })
}

async fn generated_columns(
    service: &DataSyncService,
    config: &ConnectionConfig,
    driver: DriverKind,
    namespace: &str,
    table: &str,
) -> Result<HashSet<String>> {
    let result = service
        .connection_service()
        .execute(
            config,
            &Query::new(generated_columns_query(driver, namespace, table)),
        )
        .await?;
    Ok(first_column_strings(&result).into_iter().collect())
}

async fn ineligible_identity_indexes(
    service: &DataSyncService,
    config: &ConnectionConfig,
    namespace: &str,
    table: &str,
) -> Result<HashSet<String>> {
    let namespace = namespace.replace('\'', "''");
    let table = table.replace('\'', "''");
    let sql = match config.driver {
        DriverKind::Postgres => format!(
            "SELECT i.relname::text FROM pg_index ix \
             JOIN pg_class i ON i.oid=ix.indexrelid \
             JOIN pg_class t ON t.oid=ix.indrelid \
             JOIN pg_namespace n ON n.oid=t.relnamespace \
             WHERE n.nspname='{namespace}' AND t.relname='{table}' \
               AND (ix.indpred IS NOT NULL OR ix.indexprs IS NOT NULL \
                    OR NOT ix.indisvalid OR NOT ix.indisready);"
        ),
        DriverKind::Mysql => format!(
            "SELECT DISTINCT INDEX_NAME FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA='{namespace}' AND TABLE_NAME='{table}' \
               AND SUB_PART IS NOT NULL;"
        ),
        DriverKind::Redis | DriverKind::Mongodb => {
            return Err(DomainError::InvalidConfig(
                "非 SQL 引擎不能检查 SQL 身份索引".into(),
            ));
        }
    };
    let result = service
        .connection_service()
        .execute(config, &Query::new(sql))
        .await?;
    Ok(first_column_strings(&result).into_iter().collect())
}

async fn column_collations(
    service: &DataSyncService,
    config: &ConnectionConfig,
    namespace: &str,
    table: &str,
) -> Result<HashMap<String, Option<String>>> {
    let namespace_literal = namespace.replace('\'', "''");
    let table_literal = table.replace('\'', "''");
    let namespace_ident = namespace.replace('"', "\"\"").replace('\'', "''");
    let table_ident = table.replace('"', "\"\"").replace('\'', "''");
    let sql = match config.driver {
        DriverKind::Mysql => format!(
            "SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA='{namespace_literal}' AND TABLE_NAME='{table_literal}' ORDER BY ORDINAL_POSITION;"
        ),
        DriverKind::Postgres => format!(
            "SELECT a.attname::text, CASE WHEN a.attcollation=0 THEN NULL \
                    ELSE a.attcollation::regcollation::text END \
             FROM pg_attribute a \
             WHERE a.attrelid='\"{namespace_ident}\".\"{table_ident}\"'::regclass \
               AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum;"
        ),
        DriverKind::Redis | DriverKind::Mongodb => {
            return Err(DomainError::InvalidConfig(
                "非 SQL 引擎不能检查列排序规则".into(),
            ));
        }
    };
    let result = service
        .connection_service()
        .execute(config, &Query::new(sql))
        .await?;
    let mut collations = HashMap::with_capacity(result.rows.len());
    for row in result.rows {
        let Some(Value::Text(name)) = row.values.first() else {
            return Err(DomainError::QueryFailed("列排序规则查询缺少列名".into()));
        };
        let collation = match row.values.get(1) {
            Some(Value::Text(value)) => Some(value.clone()),
            Some(Value::Null) => None,
            _ => {
                return Err(DomainError::QueryFailed(
                    "列排序规则查询结果类型异常".into(),
                ));
            }
        };
        collations.insert(name.clone(), collation);
    }
    Ok(collations)
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
        DriverKind::Redis | DriverKind::Mongodb => unreachable!(),
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
        DriverKind::Redis | DriverKind::Mongodb => false,
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
