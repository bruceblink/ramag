//! SQL 同步 MySQL/PostgreSQL DDL 加载与重写。

use super::*;

pub(super) async fn prepare_mysql_ddl(
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

pub(super) struct PostgresRawDdl {
    table: String,
    create: String,
    enums: Vec<PostgresRawEnum>,
    sequences: crate::usecases::transfer::sql_catalog::PgSequenceInfo,
    comments: Vec<String>,
    indexes: Vec<String>,
    foreign_keys: Vec<String>,
}

pub(super) async fn load_postgres_ddl(
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
pub(super) async fn prepare_postgres_ddl(
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
