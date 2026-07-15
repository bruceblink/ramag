//! 剪贴板历史 CRUD。整条 ClipItem JSON 经 Cipher 加密为 hex 落主表（preview / 来源也敏感）。
//!
//! 为支撑 100 万级历史不退化，另建三张**明文**索引表，让取最近 N / 去重 / 分页
//! 都降到 O(log N) 或 O(N_可见)，不再全表解密：
//! - `clip_by_time`：key=recency_key（越新越小，见 `recency_key`），value=uuid —— 取最近 N
//! - `clip_by_hash`：key=content_hash，value=uuid —— 指纹去重 O(log N)
//! - `clip_uuid_meta`：key=uuid，value="recency_key\thash" —— 更新/删除时反查清旧索引

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Duration, Utc};
use parking_lot::RwLock;
use redb::{
    Database, ReadableDatabase as _, ReadableTable, ReadableTableMetadata, TableDefinition,
};
use tracing::{debug, info};

use ramag_domain::entities::{ClipItem, ClipSearchResult, contains_case_insensitive};
use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;
use crate::repos::bounded_json;

const MAX_CLIP_RECORD_JSON_BYTES: usize = 80 * 1024 * 1024;
const MAX_CLIP_RECORD_HEX_BYTES: usize = (MAX_CLIP_RECORD_JSON_BYTES + 12 + 16) * 2;
const MAX_CLIP_FULL_LIST_ITEMS: usize = 100_000;
const MAX_CLIP_FULL_LIST_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLIP_MEDIA_PATHS: usize = 200_000;
const MAX_CLIP_MEDIA_PATH_BYTES: usize = 256 * 1024;
const MAX_CLIP_MEDIA_PATH_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLIP_PRUNE_BATCH: usize = 10_000;

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

fn decode(hex: &str, cipher: &Cipher) -> Result<ClipItem> {
    let json = cipher.decrypt(hex)?;
    serde_json::from_str(&json)
        .map_err(|e| DomainError::Storage(format!("反序列化剪贴条目失败：{e}")))
}

fn decode_record(uuid: &str, hex: &str, cipher: &Cipher) -> Result<ClipItem> {
    bounded_json::ensure_len(
        hex.len(),
        MAX_CLIP_RECORD_HEX_BYTES,
        &format!("剪贴条目 {uuid} 密文"),
    )?;
    decode(hex, cipher).map_err(|error| {
        DomainError::Storage(format!("读取剪贴条目 {uuid} 失败：{}", error.message()))
    })
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
        out.push(decode_record(uuid.value(), value.value(), cipher)?);
    }
    Ok(out)
}

pub(crate) fn save(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>, item: ClipItem) -> Result<()> {
    let enc = {
        let cipher = cipher.read();
        encode(&item, &cipher)?
    };
    let uuid = item.id.to_string();
    let hash = item.content_hash.clone();
    let rk = recency_key(item.last_used_at, &uuid);

    let write_txn = db.begin_write().map_err(store_err)?;
    {
        let mut clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut by_hash = write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
        let mut meta = write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;

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
        by_hash
            .insert(hash.as_str(), uuid.as_str())
            .map_err(store_err)?;
        let meta_val = encode_meta(&rk, &hash);
        meta.insert(uuid.as_str(), meta_val.as_str())
            .map_err(store_err)?;
    }
    write_txn.commit().map_err(store_err)?;
    debug!(clip_id = %uuid, "clip saved");
    Ok(())
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
    for entry in clips.iter().map_err(store_err)? {
        let (uuid, value) = entry.map_err(store_err)?;
        let item = decode_record(uuid.value(), value.value(), &cipher)?;
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
    for entry in by_time.iter().map_err(store_err)?.take(limit) {
        let (rk, uuid_g) = entry.map_err(store_err)?;
        let uuid = uuid_g.value();
        let enc_g = clips.get(uuid).map_err(store_err)?.ok_or_else(|| {
            DomainError::Storage(format!("剪贴时间索引 {} 指向缺失条目 {uuid}", rk.value()))
        })?;
        let item = decode_record(uuid, enc_g.value(), &cipher)?;
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

/// 全量搜索：时间索引最近优先遍历，解密匹配 query（preview/text，大小写不敏感），到 limit 停。
/// 早停让"最近匹配"快；罕见词最坏 O(N) 解密，但在后台、仅主动搜索时触发
pub(crate) fn search(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    query: String,
    limit: usize,
) -> Result<Vec<ClipItem>> {
    Ok(search_cancellable_bounded(
        db,
        cipher,
        query,
        limit,
        u64::MAX,
        Arc::new(AtomicBool::new(false)),
    )?
    .items)
}

/// 搜索过程中响应上层代际取消，避免过期查询继续占用存储工作线程并全表解密。
pub(crate) fn search_cancellable(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    query: String,
    limit: usize,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<ClipItem>> {
    Ok(search_cancellable_bounded(db, cipher, query, limit, u64::MAX, cancelled)?.items)
}

/// 搜索命中在加入结果向量前同时检查条数与正文预算，并返回明确截断状态。
pub(crate) fn search_cancellable_bounded(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    query: String,
    limit: usize,
    max_inline_bytes: u64,
    cancelled: Arc<AtomicBool>,
) -> Result<ClipSearchResult> {
    let q = query.trim().to_lowercase();
    if q.is_empty() || cancelled.load(Ordering::Relaxed) {
        return Ok(ClipSearchResult {
            items: Vec::new(),
            truncated: false,
        });
    }
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    let mut out = Vec::new();
    let mut total_inline_bytes = 0u64;
    let mut truncated = false;
    for entry in by_time.iter().map_err(store_err)? {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let (rk, uuid_g) = entry.map_err(store_err)?;
        let uuid = uuid_g.value();
        let enc_g = clips.get(uuid).map_err(store_err)?.ok_or_else(|| {
            DomainError::Storage(format!("剪贴时间索引 {} 指向缺失条目 {uuid}", rk.value()))
        })?;
        let item = decode_record(uuid, enc_g.value(), &cipher)?;
        let hit = contains_case_insensitive(&item.preview, &q)
            || item
                .text
                .as_deref()
                .is_some_and(|text| contains_case_insensitive(text, &q));
        if hit {
            let next_total = total_inline_bytes.saturating_add(item.inline_payload_bytes());
            let count_full = out.len() >= limit;
            let bytes_full =
                max_inline_bytes == 0 || (!out.is_empty() && next_total > max_inline_bytes);
            if count_full || bytes_full {
                truncated = true;
                break;
            }
            total_inline_bytes = next_total;
            out.push(item);
        }
    }
    Ok(ClipSearchResult {
        items: out,
        truncated,
    })
}

pub(crate) fn delete(db: Arc<Database>, id: String) -> Result<()> {
    let write_txn = db.begin_write().map_err(store_err)?;
    {
        let mut clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut by_hash = write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
        let mut meta = write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;

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
            remove_hash_if_owned(&mut by_hash, &hash, &id)?;
        }
        meta.remove(id.as_str()).map_err(store_err)?;
        clips.remove(id.as_str()).map_err(store_err)?;
    }
    write_txn.commit().map_err(store_err)?;
    debug!(clip_id = %id, "clip deleted");
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

/// 清空全部历史。返回被删条目的媒体路径（调用方负责删落盘文件）
pub(crate) fn clear(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>) -> Result<Vec<String>> {
    // 清空是损坏数据的最终恢复入口：个别记录无法解密时仍删除全部表，未知媒体由
    // 应用层随后执行的孤儿扫描清理。
    let images = match media_paths(db.clone(), cipher) {
        Ok(images) => images,
        Err(error) => {
            tracing::warn!(error = %error, "collect clip media before clear failed");
            Vec::new()
        }
    };
    let write_txn = db.begin_write().map_err(store_err)?;
    write_txn.delete_table(CLIPS_TABLE).map_err(store_err)?;
    write_txn.delete_table(CLIP_BY_TIME).map_err(store_err)?;
    write_txn.delete_table(CLIP_BY_HASH).map_err(store_err)?;
    write_txn.delete_table(CLIP_UUID_META).map_err(store_err)?;
    ensure_table(&write_txn)?;
    write_txn.commit().map_err(store_err)?;
    info!("clips cleared");
    Ok(images)
}

/// 超量 / 过期清理：扫时间索引（不解密）定位越界 / 超龄条目，只解密待删的取媒体路径
pub(crate) fn prune(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    max_items: u32,
    max_age_days: u32,
) -> Result<Vec<String>> {
    let cutoff_millis = (Utc::now() - Duration::days(i64::from(max_age_days))).timestamp_millis();

    // 快速路径：未超量 + 最旧未超龄 → 无需清理，避免正常采集每次都全表扫描索引
    {
        let read_txn = db.begin_read().map_err(store_err)?;
        let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let total = by_time.len().map_err(store_err)?;
        let oldest_over_age = match by_time.iter().map_err(store_err)?.next_back() {
            Some(entry) => {
                let (rk_g, _) = entry.map_err(store_err)?;
                millis_from_recency_key(rk_g.value())? < cutoff_millis
            }
            None => false,
        };
        if total <= u64::from(max_items) && !oldest_over_age {
            return Ok(Vec::new());
        }
    }

    let (doomed, prune_batch_full): (Vec<String>, bool) = {
        let read_txn = db.begin_read().map_err(store_err)?;
        let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut doomed = Vec::with_capacity(MAX_CLIP_PRUNE_BATCH);
        for (idx, entry) in by_time.iter().map_err(store_err)?.enumerate() {
            let (rk_g, uuid_g) = entry.map_err(store_err)?;
            let over_count = idx >= max_items as usize;
            let over_age = millis_from_recency_key(rk_g.value())? < cutoff_millis;
            if over_count || over_age {
                doomed.push(uuid_g.value().to_string());
                if doomed.len() >= MAX_CLIP_PRUNE_BATCH {
                    break;
                }
            }
        }
        let full = doomed.len() >= MAX_CLIP_PRUNE_BATCH;
        (doomed, full)
    };
    if doomed.is_empty() {
        return Ok(Vec::new());
    }

    // 只解密待删条目取媒体路径（数量有限，非全表）
    let images: Vec<String> = {
        let cipher = cipher.read();
        let read_txn = db.begin_read().map_err(store_err)?;
        let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut imgs = Vec::new();
        let mut retained_bytes = 0usize;
        for uuid in &doomed {
            let value = clips
                .get(uuid.as_str())
                .map_err(store_err)?
                .ok_or_else(|| {
                    DomainError::Storage(format!("待清理剪贴索引指向缺失条目 {uuid}"))
                })?;
            let item = decode_record(uuid, value.value(), &cipher)?;
            for path in [item.image_path, item.thumb_path].into_iter().flatten() {
                push_media_path(&mut imgs, &mut retained_bytes, path)?;
            }
        }
        imgs
    };

    let write_txn = db.begin_write().map_err(store_err)?;
    {
        let mut clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut by_hash = write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
        let mut meta = write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;
        for uuid in &doomed {
            let value = meta.get(uuid.as_str()).map_err(store_err)?.ok_or_else(|| {
                DomainError::Storage(format!("待清理剪贴条目 {uuid} 缺少索引元数据"))
            })?;
            let (rk, hash) = decode_meta(uuid, value.value())?;
            let rk = rk.to_string();
            let hash = hash.to_string();
            drop(value);
            by_time.remove(rk.as_str()).map_err(store_err)?;
            remove_hash_if_owned(&mut by_hash, &hash, uuid)?;
            meta.remove(uuid.as_str()).map_err(store_err)?;
            clips.remove(uuid.as_str()).map_err(store_err)?;
        }
    }
    write_txn.commit().map_err(store_err)?;
    info!(
        removed = doomed.len(),
        batch_full = prune_batch_full,
        max_items,
        max_age_days,
        "clips pruned"
    );
    Ok(images)
}

fn push_media_path(
    paths: &mut Vec<String>,
    retained_bytes: &mut usize,
    path: String,
) -> Result<()> {
    bounded_json::ensure_len(path.len(), MAX_CLIP_MEDIA_PATH_BYTES, "剪贴媒体路径")?;
    let (_, next_bytes) = bounded_json::next_collection_budget(
        paths.len(),
        *retained_bytes,
        path.len(),
        MAX_CLIP_MEDIA_PATHS,
        MAX_CLIP_MEDIA_PATH_TOTAL_BYTES,
        "剪贴媒体引用",
    )?;
    *retained_bytes = next_bytes;
    paths.push(path);
    Ok(())
}

/// 由 lib.rs 在 open 时调：建主表 + 三张索引表
pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
    write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
    write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;
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
    let mut migrated = 0usize;
    {
        let clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let mut by_hash = write_txn.open_table(CLIP_BY_HASH).map_err(store_err)?;
        let mut meta = write_txn.open_table(CLIP_UUID_META).map_err(store_err)?;
        // 主表逐条解密后立即写三个索引；不再额外保留整表 (uuid, key, hash) 向量。
        for entry in clips.iter().map_err(store_err)? {
            let (uuid, value) = entry.map_err(store_err)?;
            let item = decode_record(uuid.value(), value.value(), &cipher)?;
            let rk = recency_key(item.last_used_at, uuid.value());
            by_time
                .insert(rk.as_str(), uuid.value())
                .map_err(store_err)?;
            by_hash
                .insert(item.content_hash.as_str(), uuid.value())
                .map_err(store_err)?;
            meta.insert(uuid.value(), encode_meta(&rk, &item.content_hash).as_str())
                .map_err(store_err)?;
            migrated = migrated.saturating_add(1);
        }
    }
    write_txn.commit().map_err(store_err)?;
    info!(count = migrated, "clip indexes migrated");
    Ok(())
}
