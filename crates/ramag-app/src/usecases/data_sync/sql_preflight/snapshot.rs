//! SQL 目标快照与列级元数据读取。

use super::*;

pub(in crate::usecases::data_sync) async fn current_sql_target_snapshot(
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

pub(super) async fn generated_columns(
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

pub(super) async fn ineligible_identity_indexes(
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
        DriverKind::Sqlite | DriverKind::Redis | DriverKind::Mongodb => {
            return Err(DomainError::InvalidConfig(
                "当前数据库类型不能检查 SQL 身份索引".into(),
            ));
        }
    };
    let result = service
        .connection_service()
        .execute(config, &Query::new(sql))
        .await?;
    Ok(first_column_strings(&result).into_iter().collect())
}

pub(super) async fn column_collations(
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
        DriverKind::Sqlite | DriverKind::Redis | DriverKind::Mongodb => {
            return Err(DomainError::InvalidConfig(
                "当前数据库类型不能检查列排序规则".into(),
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
