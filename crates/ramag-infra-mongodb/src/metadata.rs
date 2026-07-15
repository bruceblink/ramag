//! 元数据查询：database / collection / index / stats。
//! 都用最小可用 API，避免 mongodb crate 高级 builder 链式的不稳定调用

use bson::{Bson, Document, doc};
use futures::TryStreamExt;
use mongodb::Client;
use ramag_domain::entities::{MongoCollection, MongoCollectionStats, MongoDatabase, MongoIndex};
use ramag_domain::error::{DomainError, Result};

use crate::errors::map_mongo_error;

const MAX_METADATA_ITEMS: usize = 50_000;

pub async fn list_databases(client: &Client) -> Result<Vec<MongoDatabase>> {
    let names = client
        .list_database_names()
        .await
        .map_err(map_mongo_error)?;
    ensure_metadata_item_limit(names.len(), "数据库")?;
    Ok(names
        .into_iter()
        .map(|name| MongoDatabase {
            name,
            size_on_disk: None,
            empty: false,
        })
        .collect())
}

pub async fn list_collections(client: &Client, db: &str) -> Result<Vec<MongoCollection>> {
    let database = client.database(db);
    // list_collections 返回 cursor of CollectionSpecification（含 name + type）
    let mut cursor = database.list_collections().await.map_err(map_mongo_error)?;
    let mut out = Vec::new();
    while let Some(spec) = cursor.try_next().await.map_err(map_mongo_error)? {
        ensure_metadata_item_limit(out.len().saturating_add(1), "集合")?;
        let is_view = matches!(spec.collection_type, mongodb::results::CollectionType::View);
        out.push(MongoCollection {
            name: spec.name,
            database: db.to_string(),
            is_view,
        });
    }
    // listCollections 返回服务端顺序，按集合名字典序排，左侧列表稳定有序（与 SQL 的 ORDER BY 一致）
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn ensure_metadata_item_limit(item_count: usize, label: &str) -> Result<()> {
    if item_count > MAX_METADATA_ITEMS {
        return Err(DomainError::QueryFailed(format!(
            "{label}数量超过 {MAX_METADATA_ITEMS} 条安全上限，请缩小数据库范围"
        )));
    }
    Ok(())
}

pub async fn list_indexes(client: &Client, db: &str, coll: &str) -> Result<Vec<MongoIndex>> {
    let collection = client.database(db).collection::<Document>(coll);
    let mut cursor = collection.list_indexes().await.map_err(map_mongo_error)?;
    let mut out = Vec::new();
    while let Some(model) = cursor.try_next().await.map_err(map_mongo_error)? {
        let name = model
            .options
            .as_ref()
            .and_then(|o| o.name.clone())
            .unwrap_or_else(|| "(unnamed)".to_string());
        let primary = name == "_id_";
        let unique = model
            .options
            .as_ref()
            .and_then(|o| o.unique)
            .unwrap_or(false);
        let sparse = model
            .options
            .as_ref()
            .and_then(|o| o.sparse)
            .unwrap_or(false);
        let keys = parse_index_keys(&model.keys);
        out.push(MongoIndex {
            name,
            keys,
            unique: unique || primary,
            primary,
            sparse,
        });
    }
    Ok(out)
}

pub async fn collection_stats(
    client: &Client,
    db: &str,
    coll: &str,
) -> Result<MongoCollectionStats> {
    let database = client.database(db);
    let raw: Document = database
        .run_command(doc! {"collStats": coll})
        .await
        .map_err(map_mongo_error)?;

    // collStats 字段类型按 server 版本可能是 Int32 / Int64 / Double，统一容错取值
    let count = number_field_u64(&raw, "count");
    let size_bytes = number_field_u64(&raw, "size");
    let avg_obj_size = number_field_u64(&raw, "avgObjSize");
    let storage_size = number_field_u64(&raw, "storageSize");
    let index_count = u32::try_from(number_field_u64(&raw, "nindexes")).unwrap_or(u32::MAX);

    Ok(MongoCollectionStats {
        count,
        size_bytes,
        avg_obj_size,
        storage_size,
        index_count,
    })
}

/// 跨 Int32 / Int64 / Double 取数字字段，缺失或负值返回 0
fn number_field_u64(doc: &Document, key: &str) -> u64 {
    match doc.get(key) {
        Some(Bson::Int32(i)) => (*i).max(0) as u64,
        Some(Bson::Int64(i)) => (*i).max(0) as u64,
        Some(Bson::Double(d)) => d.max(0.0) as u64,
        _ => 0,
    }
}

/// 索引 keys 的 BSON Document 转 (field, direction)。
/// 普通 1/-1 转 i32；其它（"text" / "2dsphere" / "hashed"）按 0 占位
fn parse_index_keys(keys: &Document) -> Vec<(String, i32)> {
    keys.iter()
        .map(|(k, v)| {
            let dir = match v {
                Bson::Int32(i) => *i,
                Bson::Int64(i) => i32::try_from(*i).unwrap_or(0),
                Bson::Double(d)
                    if d.is_finite()
                        && d.fract() == 0.0
                        && *d >= i32::MIN as f64
                        && *d <= i32::MAX as f64 =>
                {
                    *d as i32
                }
                _ => 0,
            };
            (k.clone(), dir)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keys_basic() {
        let mut d = Document::new();
        d.insert("name", Bson::Int32(1));
        d.insert("age", Bson::Int32(-1));
        let keys = parse_index_keys(&d);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], ("name".into(), 1));
        assert_eq!(keys[1], ("age".into(), -1));
    }

    #[test]
    fn parse_keys_text_index_zero_dir() {
        let mut d = Document::new();
        d.insert("title", Bson::String("text".into()));
        let keys = parse_index_keys(&d);
        assert_eq!(keys[0], ("title".into(), 0));
    }

    #[test]
    fn parse_keys_does_not_wrap_out_of_range_numbers() {
        let mut d = Document::new();
        d.insert("large", Bson::Int64(i64::from(i32::MAX) + 1));
        d.insert("fractional", Bson::Double(1.5));
        d.insert("infinite", Bson::Double(f64::INFINITY));

        let keys = parse_index_keys(&d);

        assert_eq!(keys[0], ("large".into(), 0));
        assert_eq!(keys[1], ("fractional".into(), 0));
        assert_eq!(keys[2], ("infinite".into(), 0));
    }

    #[test]
    fn metadata_limit_allows_boundary_and_rejects_overflow() {
        assert!(ensure_metadata_item_limit(MAX_METADATA_ITEMS, "集合").is_ok());
        assert!(ensure_metadata_item_limit(MAX_METADATA_ITEMS + 1, "集合").is_err());
    }
}
