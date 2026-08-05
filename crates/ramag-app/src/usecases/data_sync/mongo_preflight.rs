//! MongoDB 同步预检：范围展开、Collection 选项与索引兼容性、目标指纹。

use std::collections::HashMap;

use ramag_domain::entities::{
    ConnectionConfig, DataSyncRequest, DriverKind, MongoCollection, MongoSyncScope,
    SyncObjectMapping, SyncObjectSelection, SyncObjectState, SyncPlannedObject,
};
use ramag_domain::error::{DomainError, Result};
use serde_json::{Map, Value, json};

use super::service::{
    DataSyncPreflightReport, DataSyncService, MongoCollectionBlueprint, MongoPreparedObject,
    MongoPreparedPlan, MongoTargetSnapshot, PreparedDataSync, PreparedEnginePlan, fingerprint,
};

pub(super) async fn preflight_mongo(
    service: &DataSyncService,
    request: DataSyncRequest,
    source: ConnectionConfig,
    target: ConnectionConfig,
    scope: MongoSyncScope,
) -> Result<PreparedDataSync> {
    reject_protected_database(&scope.target_database)?;
    let mongo = service.mongo_service();
    let (source_version, target_version) =
        futures::join!(mongo.server_version(&source), mongo.server_version(&target));
    let source_version = source_version.map_err(|error| {
        DomainError::ConnectionFailed(format!("源 MongoDB 连接预检失败：{}", error.message()))
    })?;
    let target_version = target_version.map_err(|error| {
        DomainError::ConnectionFailed(format!("目标 MongoDB 连接预检失败：{}", error.message()))
    })?;

    let source_collections = mongo
        .list_collections(&source, &scope.source_database)
        .await?;
    let mappings = expand_mappings(&scope.collections, &source_collections)?;
    let target_databases = mongo.list_databases(&target).await?;
    let database_exists = target_databases
        .iter()
        .any(|database| database.name == scope.target_database);
    let target_collections = if database_exists {
        mongo
            .list_collections(&target, &scope.target_database)
            .await?
    } else {
        Vec::new()
    };
    let target_by_name: HashMap<&str, &MongoCollection> = target_collections
        .iter()
        .map(|collection| (collection.name.as_str(), collection))
        .collect();

    let mut prepared_objects = Vec::with_capacity(mappings.len());
    let mut report_objects = Vec::with_capacity(mappings.len());
    let mut snapshot_collections = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        let source_blueprint =
            collection_blueprint(service, &source, &scope.source_database, &mapping.source).await?;
        let target_collection = target_by_name.get(mapping.target.as_str()).copied();
        if target_collection.is_some_and(|collection| collection.is_view) {
            return Err(DomainError::InvalidConfig(format!(
                "目标 {}.{} 是视图，不能写入同步数据",
                scope.target_database, mapping.target
            )));
        }
        let target_blueprint = match target_collection {
            Some(_) => Some(
                collection_blueprint(service, &target, &scope.target_database, &mapping.target)
                    .await?,
            ),
            None => None,
        };
        let missing_indexes = match &target_blueprint {
            Some(target_blueprint) => {
                ensure_collection_compatible(&mapping, &source_blueprint, target_blueprint)?;
                missing_indexes(&source_blueprint.indexes, &target_blueprint.indexes)?
            }
            None => source_blueprint.indexes.clone(),
        };
        report_objects.push(SyncPlannedObject {
            mapping: mapping.clone(),
            state: if target_blueprint.is_some() {
                SyncObjectState::ExistingCompatible
            } else {
                SyncObjectState::Missing
            },
        });
        snapshot_collections.push((mapping.target.clone(), target_blueprint.clone()));
        prepared_objects.push(MongoPreparedObject {
            mapping,
            source_blueprint,
            missing_indexes,
            target_exists: target_blueprint.is_some(),
        });
    }

    let target_snapshot = MongoTargetSnapshot {
        database_exists,
        collections: snapshot_collections,
    };
    let mut report = DataSyncPreflightReport {
        task_id: request.task_id.clone(),
        engine: DriverKind::Mongodb,
        source_connection: source.name.clone(),
        source_scope: format!("Database {}", scope.source_database),
        source_version,
        target_connection: target.name.clone(),
        target_scope: format!("Database {}", scope.target_database),
        target_version,
        objects_total: Some(prepared_objects.len() as u64),
        objects: report_objects,
        target_scope_exists: database_exists,
        requires_second_confirmation: database_exists,
        target_fingerprint: fingerprint(&target_snapshot)?,
        warnings: Vec::new(),
        warnings_overflow: 0,
    };
    if prepared_objects.is_empty() {
        report.push_warning("源 MongoDB 范围当前没有 Collection");
    }
    if prepared_objects
        .iter()
        .any(|object| object.target_exists && !object.missing_indexes.is_empty())
    {
        report.push_warning("已有目标 Collection 缺少部分源索引，数据完成后将补建索引");
    }
    Ok(PreparedDataSync {
        request,
        source,
        target,
        report,
        engine_plan: PreparedEnginePlan::Mongo(MongoPreparedPlan {
            scope,
            objects: prepared_objects,
            target_snapshot,
        }),
    })
}

pub(super) async fn current_mongo_target_snapshot(
    service: &DataSyncService,
    target: &ConnectionConfig,
    plan: &MongoPreparedPlan,
) -> Result<MongoTargetSnapshot> {
    let databases = service.mongo_service().list_databases(target).await?;
    let database_exists = databases
        .iter()
        .any(|database| database.name == plan.scope.target_database);
    let collections = if database_exists {
        service
            .mongo_service()
            .list_collections(target, &plan.scope.target_database)
            .await?
    } else {
        Vec::new()
    };
    let by_name: HashMap<&str, &MongoCollection> = collections
        .iter()
        .map(|collection| (collection.name.as_str(), collection))
        .collect();
    let mut snapshots = Vec::with_capacity(plan.objects.len());
    for object in &plan.objects {
        let blueprint = if let Some(collection) = by_name.get(object.mapping.target.as_str()) {
            if collection.is_view {
                return Err(DomainError::InvalidConfig(format!(
                    "目标 {}.{} 已变为视图",
                    plan.scope.target_database, object.mapping.target
                )));
            }
            Some(
                collection_blueprint(
                    service,
                    target,
                    &plan.scope.target_database,
                    &object.mapping.target,
                )
                .await?,
            )
        } else {
            None
        };
        snapshots.push((object.mapping.target.clone(), blueprint));
    }
    Ok(MongoTargetSnapshot {
        database_exists,
        collections: snapshots,
    })
}

fn expand_mappings(
    selection: &SyncObjectSelection,
    source_collections: &[MongoCollection],
) -> Result<Vec<SyncObjectMapping>> {
    let source_by_name: HashMap<&str, &MongoCollection> = source_collections
        .iter()
        .map(|collection| (collection.name.as_str(), collection))
        .collect();
    match selection {
        SyncObjectSelection::All => {
            if let Some(view) = source_collections
                .iter()
                .find(|collection| collection.is_view)
            {
                return Err(DomainError::InvalidConfig(format!(
                    "MongoDB 视图 {} 不在整库数据同步范围内，请改用 Collection 级范围",
                    view.name
                )));
            }
            Ok(source_collections
                .iter()
                .map(|collection| SyncObjectMapping {
                    source: collection.name.clone(),
                    target: collection.name.clone(),
                })
                .collect())
        }
        SyncObjectSelection::Selected(mappings) => {
            for mapping in mappings {
                let collection = source_by_name.get(mapping.source.as_str()).ok_or_else(|| {
                    DomainError::NotFound(format!("源 Collection 不存在：{}", mapping.source))
                })?;
                if collection.is_view {
                    return Err(DomainError::InvalidConfig(format!(
                        "MongoDB 视图 {} 不支持数据同步",
                        mapping.source
                    )));
                }
            }
            Ok(mappings.clone())
        }
    }
}

pub(super) async fn collection_blueprint(
    service: &DataSyncService,
    config: &ConnectionConfig,
    database: &str,
    collection: &str,
) -> Result<MongoCollectionBlueprint> {
    let collection_result = service
        .mongo_service()
        .run_command(
            config,
            database,
            json!({"listCollections": 1, "filter": {"name": collection}, "nameOnly": false}),
        )
        .await?;
    let collections = cursor_batch(&collection_result, "listCollections")?;
    let spec = collections
        .iter()
        .find(|spec| spec.get("name").and_then(Value::as_str) == Some(collection))
        .ok_or_else(|| {
            DomainError::NotFound(format!("Collection {database}.{collection} 不存在"))
        })?;
    if spec.get("type").and_then(Value::as_str) == Some("view") {
        return Err(DomainError::InvalidConfig(format!(
            "MongoDB 视图 {database}.{collection} 不支持数据同步"
        )));
    }
    let options = normalize_collection_options(spec.get("options"));

    let index_result = service
        .mongo_service()
        .run_command(config, database, json!({"listIndexes": collection}))
        .await?;
    let mut indexes: Vec<Value> = cursor_batch(&index_result, "listIndexes")?
        .iter()
        .filter_map(normalize_index)
        .filter(|index| index.get("name").and_then(Value::as_str) != Some("_id_"))
        .collect();
    indexes.sort_by(|left, right| index_name(left).cmp(&index_name(right)));
    Ok(MongoCollectionBlueprint { options, indexes })
}

fn normalize_collection_options(value: Option<&Value>) -> Value {
    const ALLOWED: &[&str] = &[
        "capped",
        "size",
        "max",
        "validator",
        "validationLevel",
        "validationAction",
        "storageEngine",
        "collation",
        "timeseries",
        "expireAfterSeconds",
        "clusteredIndex",
        "changeStreamPreAndPostImages",
    ];
    let mut normalized = Map::new();
    if let Some(options) = value.and_then(Value::as_object) {
        for key in ALLOWED {
            if let Some(value) = options.get(*key) {
                normalized.insert((*key).to_string(), value.clone());
            }
        }
    }
    Value::Object(normalized)
}

fn normalize_index(value: &Value) -> Option<Value> {
    const ALLOWED: &[&str] = &[
        "key",
        "name",
        "unique",
        "sparse",
        "expireAfterSeconds",
        "partialFilterExpression",
        "collation",
        "wildcardProjection",
        "hidden",
        "weights",
        "default_language",
        "language_override",
        "textIndexVersion",
        "2dsphereIndexVersion",
        "bits",
        "min",
        "max",
        "bucketSize",
        "storageEngine",
    ];
    let object = value.as_object()?;
    if !object.contains_key("key") || !object.contains_key("name") {
        return None;
    }
    let mut normalized = Map::new();
    for key in ALLOWED {
        if let Some(value) = object.get(*key) {
            normalized.insert((*key).to_string(), value.clone());
        }
    }
    Some(Value::Object(normalized))
}

fn ensure_collection_compatible(
    mapping: &SyncObjectMapping,
    source: &MongoCollectionBlueprint,
    target: &MongoCollectionBlueprint,
) -> Result<()> {
    if source.options != target.options {
        return Err(DomainError::InvalidConfig(format!(
            "目标 Collection {} 的创建选项与源 {} 不兼容",
            mapping.target, mapping.source
        )));
    }
    let target_indexes: HashMap<&str, &Value> = target
        .indexes
        .iter()
        .filter_map(|index| Some((index_name(index)?, index)))
        .collect();
    for source_index in &source.indexes {
        let Some(name) = index_name(source_index) else {
            return Err(DomainError::QueryFailed("源 MongoDB 索引缺少名称".into()));
        };
        if let Some(target_index) = target_indexes.get(name)
            && *target_index != source_index
        {
            return Err(DomainError::InvalidConfig(format!(
                "目标 Collection {} 的索引 {name} 与源定义不同",
                mapping.target
            )));
        }
    }
    Ok(())
}

fn missing_indexes(source: &[Value], target: &[Value]) -> Result<Vec<Value>> {
    let target_by_name: HashMap<&str, &Value> = target
        .iter()
        .filter_map(|index| Some((index_name(index)?, index)))
        .collect();
    let mut missing = Vec::new();
    for index in source {
        let name = index_name(index)
            .ok_or_else(|| DomainError::QueryFailed("源 MongoDB 索引缺少名称".into()))?;
        if !target_by_name.contains_key(name) {
            missing.push(index.clone());
        }
    }
    Ok(missing)
}

fn cursor_batch<'a>(response: &'a Value, command: &str) -> Result<&'a [Value]> {
    if response
        .get("__ramag_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(DomainError::QueryFailed(format!(
            "MongoDB {command} 元数据超过安全边界，不能基于不完整结构执行同步"
        )));
    }
    response
        .get("cursor")
        .and_then(|cursor| cursor.get("firstBatch"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            DomainError::QueryFailed(format!("MongoDB {command} 应答缺少 cursor.firstBatch"))
        })
}

fn index_name(index: &Value) -> Option<&str> {
    index.get("name").and_then(Value::as_str)
}

fn reject_protected_database(database: &str) -> Result<()> {
    if matches!(
        database.to_ascii_lowercase().as_str(),
        "admin" | "config" | "local"
    ) {
        return Err(DomainError::Forbidden(format!(
            "目标 Database {database} 是 MongoDB 系统库，禁止数据同步"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_options_drop_server_generated_fields() {
        let options = normalize_collection_options(Some(&json!({
            "validator": {"age": {"$gte": 0}},
            "uuid": "server-generated",
            "readOnly": false
        })));
        assert!(options.get("validator").is_some());
        assert!(options.get("uuid").is_none());
        assert!(options.get("readOnly").is_none());
    }

    #[test]
    fn index_normalization_is_stable_and_excludes_namespace_fields() {
        let index = normalize_index(&json!({
            "v": 2,
            "key": {"email": 1},
            "name": "email_1",
            "unique": true,
            "ns": "app.users"
        }))
        .expect("索引应可归一化");
        assert_eq!(index.get("name").and_then(Value::as_str), Some("email_1"));
        assert!(index.get("unique").is_some());
        assert!(index.get("v").is_none());
        assert!(index.get("ns").is_none());
    }

    #[test]
    fn incompatible_same_name_index_is_rejected() {
        let mapping = SyncObjectMapping {
            source: "source".into(),
            target: "target".into(),
        };
        let source = MongoCollectionBlueprint {
            options: json!({}),
            indexes: vec![json!({"key": {"email": 1}, "name": "email_1", "unique": true})],
        };
        let target = MongoCollectionBlueprint {
            options: json!({}),
            indexes: vec![json!({"key": {"email": 1}, "name": "email_1"})],
        };
        assert!(ensure_collection_compatible(&mapping, &source, &target).is_err());
    }

    #[test]
    fn system_databases_are_rejected() {
        assert!(reject_protected_database("admin").is_err());
        assert!(reject_protected_database("application").is_ok());
    }

    #[test]
    fn truncated_catalog_response_is_rejected() {
        let response = json!({
            "cursor": {"firstBatch": [{"name": "partial"}]},
            "__ramag_truncated": true
        });
        let error =
            cursor_batch(&response, "listCollections").expect_err("不完整元数据不能进入同步计划");
        assert!(error.message().contains("不完整结构"));
    }
}
