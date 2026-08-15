//! 加密剪贴板历史及其时间、内容哈希和搜索索引。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use redb::{
    Database, ReadableDatabase as _, ReadableTable, ReadableTableMetadata, TableDefinition,
};
use tracing::{debug, info};

use ramag_domain::entities::ClipItem;
use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;
use crate::repos::bounded_json;

const MAX_CLIP_RECORD_JSON_BYTES: usize = 80 * 1024 * 1024;
const MAX_CLIP_RECORD_HEX_BYTES: usize = (MAX_CLIP_RECORD_JSON_BYTES + 12 + 16) * 2;
const MAX_CLIP_FULL_LIST_ITEMS: usize = 100_000;
const MAX_CLIP_FULL_LIST_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLIP_PRUNE_BATCH: usize = 10_000;

mod maintenance;
mod prune;
mod search;
use maintenance::push_media_path;
pub(crate) use maintenance::{clear, prune};
pub(crate) use search::{
    initialize_index as initialize_search_index, search, search_cancellable,
    search_cancellable_bounded,
};

/// 主表：key=ClipId UUID，value=加密 JSON（hex）
pub(crate) const CLIPS_TABLE: TableDefinition<&str, &str> = TableDefinition::new("clips");
/// 时间索引：key=recency_key，value=uuid（按最近优先有序）
pub(crate) const CLIP_BY_TIME: TableDefinition<&str, &str> = TableDefinition::new("clip_by_time");
/// 去重索引：key=content_hash，value=uuid
pub(crate) const CLIP_BY_HASH: TableDefinition<&str, &str> = TableDefinition::new("clip_by_hash");
/// 反查表：key=uuid，value="recency_key\thash"（更新/删除时定位旧索引项）
pub(crate) const CLIP_UUID_META: TableDefinition<&str, &str> =
    TableDefinition::new("clip_uuid_meta");

fn store_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::Storage(e.to_string())
}

fn encode(item: &ClipItem, cipher: &Cipher) -> Result<String> {
    let json = bounded_json::serialize(item, MAX_CLIP_RECORD_JSON_BYTES, "剪贴条目")?;
    cipher.encrypt(&json)
}

fn decode_record(uuid: &str, hex: &str, cipher: &Cipher) -> Result<ClipItem> {
    decode_record_reusing(uuid, hex, cipher, &mut Vec::new())
}

fn decode_record_reusing(
    uuid: &str,
    hex: &str,
    cipher: &Cipher,
    scratch: &mut Vec<u8>,
) -> Result<ClipItem> {
    bounded_json::ensure_len(
        hex.len(),
        MAX_CLIP_RECORD_HEX_BYTES,
        &format!("剪贴条目 {uuid} 密文"),
    )?;
    let json = cipher.decrypt_hex_into(hex, scratch).map_err(|error| {
        DomainError::Storage(format!("读取剪贴条目 {uuid} 失败：{}", error.message()))
    })?;
    serde_json::from_slice(json)
        .map_err(|error| DomainError::Storage(format!("反序列化剪贴条目 {uuid} 失败：{error}")))
}

/// 启动时解密一条主记录，尽早发现系统凭据与数据库不匹配。
pub(crate) fn validate_key(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>) -> Result<()> {
    let read_txn = db.begin_read().map_err(store_err)?;
    let table = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    if let Some(entry) = table.iter().map_err(store_err)?.next() {
        let (uuid, value) = entry.map_err(store_err)?;
        let _ = decode_record(uuid.value(), value.value(), &cipher.read())?;
    }
    Ok(())
}

/// 最近优先的有序 key：`{inverted_millis:016x}:{uuid}`。
/// inverted = u64::MAX - last_used_millis → 越新越小 → 升序遍历即最近优先；
/// 拼 uuid 保证同毫秒多条不冲突。定长 16 位 hex 让字典序 == 数值序。
fn recency_key(last_used: DateTime<Utc>, uuid: &str) -> String {
    let millis = last_used.timestamp_millis().max(0) as u64;
    let inverted = u64::MAX - millis;
    format!("{inverted:016x}:{uuid}")
}

/// 从 recency_key 反解出 last_used 毫秒（prune 判超龄用，无需解密条目）
fn millis_from_recency_key(rk: &str) -> Result<i64> {
    let (hex, uuid) = rk
        .split_once(':')
        .ok_or_else(|| DomainError::Storage(format!("剪贴时间索引格式无效：{rk}")))?;
    if uuid.is_empty() || hex.len() != 16 {
        return Err(DomainError::Storage(format!("剪贴时间索引格式无效：{rk}")));
    }
    let inverted = u64::from_str_radix(hex, 16)
        .map_err(|error| DomainError::Storage(format!("剪贴时间索引格式无效 {rk}：{error}")))?;
    Ok((u64::MAX - inverted) as i64)
}

fn encode_meta(rk: &str, hash: &str) -> String {
    format!("{rk}\t{hash}")
}

fn decode_meta<'a>(uuid: &str, value: &'a str) -> Result<(&'a str, &'a str)> {
    let (rk, hash) = value
        .split_once('\t')
        .ok_or_else(|| DomainError::Storage(format!("剪贴索引元数据 {uuid} 格式无效")))?;
    if rk.is_empty() || hash.is_empty() {
        return Err(DomainError::Storage(format!(
            "剪贴索引元数据 {uuid} 格式无效"
        )));
    }
    Ok((rk, hash))
}

fn remove_hash_if_owned(
    by_hash: &mut redb::Table<'_, &str, &str>,
    hash: &str,
    uuid: &str,
) -> Result<()> {
    let owned = by_hash
        .get(hash)
        .map_err(store_err)?
        .is_some_and(|value| value.value() == uuid);
    if owned {
        by_hash.remove(hash).map_err(store_err)?;
    }
    Ok(())
}

/// 全表解密（仅 clear / cleanup 等低频全量场景用，不在采集 / 唤起热路径）
fn load_all(db: &Arc<Database>, cipher: &Cipher) -> Result<Vec<ClipItem>> {
    let read_txn = db.begin_read().map_err(store_err)?;
    let table = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    let mut out = Vec::new();
    let mut retained_bytes = 0usize;
    let mut scratch = Vec::new();
    for entry in table.iter().map_err(store_err)? {
        let (uuid, value) = entry.map_err(store_err)?;
        let (_, next_bytes) = bounded_json::next_collection_budget(
            out.len(),
            retained_bytes,
            value.value().len(),
            MAX_CLIP_FULL_LIST_ITEMS,
            MAX_CLIP_FULL_LIST_BYTES,
            "剪贴板全量列表",
        )?;
        retained_bytes = next_bytes;
        out.push(decode_record_reusing(
            uuid.value(),
            value.value(),
            cipher,
            &mut scratch,
        )?);
    }
    Ok(out)
}

pub(crate) fn save(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>, item: ClipItem) -> Result<()> {
    let uuid = item.id.to_string();
    let hash = item.content_hash.clone();
    let rk = recency_key(item.last_used_at, &uuid);
    let (enc, search_filter) = {
        let cipher = cipher.read();
        (
            encode(&item, &cipher)?,
            search::build_filter(&item, &cipher),
        )
    };

    let write_txn = db.begin_write().map_err(store_err)?;
    {
        let mut clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut by_hash = write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
        let mut meta = write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;
        let mut search_filters = write_txn
            .open_table(search::CLIP_SEARCH_FILTERS)
            .map_err(store_err)?;

        // 已存在（更新 last_used）→ 删旧时间索引项（recency_key 已变）
        let old_meta = match meta.get(uuid.as_str()).map_err(store_err)? {
            Some(value) => {
                let (old_rk, old_hash) = decode_meta(&uuid, value.value())?;
                Some((old_rk.to_string(), old_hash.to_string()))
            }
            None if clips.get(uuid.as_str()).map_err(store_err)?.is_some() => {
                return Err(DomainError::Storage(format!(
                    "剪贴条目 {uuid} 缺少索引元数据，拒绝覆盖"
                )));
            }
            None => None,
        };
        if let Some((old_rk, old_hash)) = old_meta {
            by_time.remove(old_rk.as_str()).map_err(store_err)?;
            search_filters.remove(old_rk.as_str()).map_err(store_err)?;
            if old_hash != hash {
                remove_hash_if_owned(&mut by_hash, &old_hash, &uuid)?;
            }
        }

        clips
            .insert(uuid.as_str(), enc.as_str())
            .map_err(store_err)?;
        by_time
            .insert(rk.as_str(), uuid.as_str())
            .map_err(store_err)?;
        search_filters
            .insert(rk.as_str(), search_filter.as_slice())
            .map_err(store_err)?;
        by_hash
            .insert(hash.as_str(), uuid.as_str())
            .map_err(store_err)?;
        let meta_val = encode_meta(&rk, &hash);
        meta.insert(uuid.as_str(), meta_val.as_str())
            .map_err(store_err)?;
    }
    write_txn.commit().map_err(store_err)?;
    debug!(operation = "clipboard_entry_save", clip_id = %uuid, "clipboard entry saved");
    Ok(())
}

pub(crate) fn get(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    uuid: String,
) -> Result<Option<ClipItem>> {
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    let Some(value) = clips.get(uuid.as_str()).map_err(store_err)? else {
        return Ok(None);
    };
    Ok(Some(decode_record(&uuid, value.value(), &cipher)?))
}

/// 全量列表（按 last_used desc）。仅 cleanup_orphans 等全量场景用；日常用 `list_recent`
pub(crate) fn list(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>) -> Result<Vec<ClipItem>> {
    let cipher = cipher.read();
    let mut out = load_all(&db, &cipher)?;
    out.sort_by_key(|i| std::cmp::Reverse(i.last_used_at));
    Ok(out)
}

/// 流式解密并仅保留媒体路径，避免孤儿清理为文本历史构造百万级 `ClipItem` 向量。
pub(crate) fn media_paths(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>) -> Result<Vec<String>> {
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    let mut paths = Vec::new();
    let mut retained_bytes = 0usize;
    let mut scratch = Vec::new();
    for entry in clips.iter().map_err(store_err)? {
        let (uuid, value) = entry.map_err(store_err)?;
        let item = decode_record_reusing(uuid.value(), value.value(), &cipher, &mut scratch)?;
        for path in [item.image_path, item.thumb_path].into_iter().flatten() {
            push_media_path(&mut paths, &mut retained_bytes, path)?;
        }
    }
    Ok(paths)
}

/// 取最近 limit 条：扫时间索引前 limit 个（已最近优先），只解密这 limit 条。O(limit)
pub(crate) fn list_recent(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    limit: usize,
) -> Result<Vec<ClipItem>> {
    list_recent_bounded(db, cipher, limit, u64::MAX)
}

/// 按最近顺序解密连续前缀，并在构造结果向量时执行正文总量预算，避免启动预热峰值失控。
pub(crate) fn list_recent_bounded(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    limit: usize,
    max_inline_bytes: u64,
) -> Result<Vec<ClipItem>> {
    if max_inline_bytes == 0 {
        return Ok(Vec::new());
    }
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    let mut out = Vec::new();
    let mut total_inline_bytes = 0u64;
    let mut scratch = Vec::new();
    for entry in by_time.iter().map_err(store_err)?.take(limit) {
        let (rk, uuid_g) = entry.map_err(store_err)?;
        let uuid = uuid_g.value();
        let enc_g = clips.get(uuid).map_err(store_err)?.ok_or_else(|| {
            DomainError::Storage(format!("剪贴时间索引 {} 指向缺失条目 {uuid}", rk.value()))
        })?;
        let item = decode_record_reusing(uuid, enc_g.value(), &cipher, &mut scratch)?;
        let next_total = total_inline_bytes.saturating_add(item.inline_payload_bytes());
        if !out.is_empty() && next_total > max_inline_bytes {
            break;
        }
        total_inline_bytes = next_total;
        out.push(item);
        if total_inline_bytes >= max_inline_bytes {
            break;
        }
    }
    Ok(out)
}

pub(crate) fn delete(db: Arc<Database>, id: String) -> Result<()> {
    let write_txn = db.begin_write().map_err(store_err)?;
    {
        let mut clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut by_hash = write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
        let mut meta = write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;
        let mut search_filters = write_txn
            .open_table(search::CLIP_SEARCH_FILTERS)
            .map_err(store_err)?;

        let clip_exists = clips.get(id.as_str()).map_err(store_err)?.is_some();
        let info = match meta.get(id.as_str()).map_err(store_err)? {
            Some(value) => {
                let (rk, hash) = decode_meta(&id, value.value())?;
                Some((rk.to_string(), hash.to_string()))
            }
            None if clip_exists => {
                return Err(DomainError::Storage(format!(
                    "剪贴条目 {id} 缺少索引元数据，拒绝不完整删除"
                )));
            }
            None => None,
        };
        if let Some((rk, hash)) = info {
            by_time.remove(rk.as_str()).map_err(store_err)?;
            search_filters.remove(rk.as_str()).map_err(store_err)?;
            remove_hash_if_owned(&mut by_hash, &hash, &id)?;
        }
        meta.remove(id.as_str()).map_err(store_err)?;
        clips.remove(id.as_str()).map_err(store_err)?;
    }
    write_txn.commit().map_err(store_err)?;
    debug!(operation = "clipboard_entry_delete", clip_id = %id, "clipboard entry deleted");
    Ok(())
}

/// 内容指纹查重：查去重索引拿 uuid → 解密该一条。O(log N)，不全表解密
pub(crate) fn find_by_hash(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    hash: String,
) -> Result<Option<ClipItem>> {
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let by_hash = read_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
    let Some(uuid_g) = by_hash.get(hash.as_str()).map_err(store_err)? else {
        return Ok(None);
    };
    let uuid = uuid_g.value().to_string();
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    match clips.get(uuid.as_str()).map_err(store_err)? {
        Some(enc_g) => Ok(Some(decode_record(&uuid, enc_g.value(), &cipher)?)),
        None => Err(DomainError::Storage(format!(
            "剪贴哈希索引 {hash} 指向缺失条目 {uuid}"
        ))),
    }
}

/// 由 lib.rs 在 open 时调：建主表与派生索引表。
pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
    write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
    write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;
    write_txn
        .open_table(search::CLIP_SEARCH_FILTERS)
        .map_err(store_err)?;
    write_txn
        .open_table(search::CLIP_SEARCH_META)
        .map_err(store_err)?;
    Ok(())
}

/// 首启迁移：主表非空但时间索引为空（旧版本数据 / 索引缺失）→ 解密全部重建三索引。
/// 一次性，之后索引随写操作在线维护
pub(crate) fn migrate_indexes(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>) -> Result<()> {
    let need = {
        let read_txn = db.begin_read().map_err(store_err)?;
        let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        clips.len().map_err(store_err)? > 0 && by_time.len().map_err(store_err)? == 0
    };
    if !need {
        return Ok(());
    }

    let cipher = cipher.read();
    let write_txn = db.begin_write().map_err(store_err)?;
    write_txn
        .delete_table(search::CLIP_SEARCH_FILTERS)
        .map_err(store_err)?;
    let mut migrated = 0usize;
    {
        let clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut by_hash = write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
        let mut meta = write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;
        let mut search_filters = write_txn
            .open_table(search::CLIP_SEARCH_FILTERS)
            .map_err(store_err)?;
        let mut scratch = Vec::new();
        // 主表逐条解密后立即写三个索引；不再额外保留整表 (uuid, key, hash) 向量。
        for entry in clips.iter().map_err(store_err)? {
            let (uuid, value) = entry.map_err(store_err)?;
            let item = decode_record_reusing(uuid.value(), value.value(), &cipher, &mut scratch)?;
            let rk = recency_key(item.last_used_at, uuid.value());
            by_time
                .insert(rk.as_str(), uuid.value())
                .map_err(store_err)?;
            let search_filter = search::build_filter(&item, &cipher);
            search_filters
                .insert(rk.as_str(), search_filter.as_slice())
                .map_err(store_err)?;
            by_hash
                .insert(item.content_hash.as_str(), uuid.value())
                .map_err(store_err)?;
            meta.insert(uuid.value(), encode_meta(&rk, &item.content_hash).as_str())
                .map_err(store_err)?;
            migrated = migrated.saturating_add(1);
        }
    }
    search::mark_ready(&write_txn)?;
    write_txn.commit().map_err(store_err)?;
    info!(
        operation = "clipboard_index_migrate",
        count = migrated,
        "clipboard indexes migrated"
    );
    Ok(())
}

#[cfg(test)]
mod performance_tests;
