//! SQL 同步外键、目标对象与身份列预检规则。

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn validate_foreign_dependencies(
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
pub(super) async fn validate_existing_target(
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

pub(super) fn missing_source_column_error(
    namespace: &str,
    table: &str,
    column: &str,
) -> DomainError {
    DomainError::InvalidConfig(format!(
        "目标表 {namespace}.{table} 缺少源列 {column}。请改用新目标表名、取消选择该表，或先补齐目标表结构"
    ))
}

pub(super) async fn postgres_identity_modes(
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
