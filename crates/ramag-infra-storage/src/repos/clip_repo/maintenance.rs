use std::sync::Arc;

use chrono::{Duration, Utc};
use parking_lot::RwLock;
use redb::{Database, ReadableDatabase as _, ReadableTable as _, ReadableTableMetadata as _};
use tracing::info;

use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;
use crate::repos::bounded_json;

use super::prune::{PruneSelection, select_prune_candidates};
use super::{
    CLIP_BY_HASH, CLIP_BY_TIME, CLIP_UUID_META, CLIPS_TABLE, decode_meta, decode_record_reusing,
    ensure_table, millis_from_recency_key, remove_hash_if_owned, search, store_err,
};

const MAX_CLIP_MEDIA_PATHS: usize = 200_000;
const MAX_CLIP_MEDIA_PATH_BYTES: usize = 256 * 1024;
const MAX_CLIP_MEDIA_PATH_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// 清空历史；媒体文件由应用层流式清理。
pub(crate) fn clear(db: Arc<Database>) -> Result<()> {
    let write_txn = db.begin_write().map_err(store_err)?;
    write_txn.delete_table(CLIPS_TABLE).map_err(store_err)?;
    write_txn.delete_table(CLIP_BY_TIME).map_err(store_err)?;
    write_txn.delete_table(CLIP_BY_HASH).map_err(store_err)?;
    write_txn.delete_table(CLIP_UUID_META).map_err(store_err)?;
    write_txn
        .delete_table(search::CLIP_SEARCH_FILTERS)
        .map_err(store_err)?;
    write_txn
        .delete_table(search::CLIP_SEARCH_META)
        .map_err(store_err)?;
    ensure_table(&write_txn)?;
    search::mark_ready(&write_txn)?;
    write_txn.commit().map_err(store_err)?;
    info!(
        operation = "clipboard_history_clear",
        "clipboard entries cleared"
    );
    Ok(())
}

/// 按数量和保存期限批量清理历史。
pub(crate) fn prune(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    max_items: u32,
    max_age_days: u32,
) -> Result<Vec<String>> {
    let cutoff_millis = (Utc::now() - Duration::days(i64::from(max_age_days))).timestamp_millis();
    let PruneSelection {
        doomed,
        batch_full: prune_batch_full,
        scanned,
    } = {
        let read_txn = db.begin_read().map_err(store_err)?;
        let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let total = by_time.len().map_err(store_err)?;
        // 常规采集无需扫描完整索引。
        let oldest_over_age = match by_time.iter().map_err(store_err)?.next_back() {
            Some(entry) => {
                let (recency_key, _) = entry.map_err(store_err)?;
                millis_from_recency_key(recency_key.value())? < cutoff_millis
            }
            None => false,
        };
        if total <= u64::from(max_items) && !oldest_over_age {
            return Ok(Vec::new());
        }
        let excess = total.saturating_sub(u64::from(max_items));
        select_prune_candidates(&by_time, excess, cutoff_millis)?
    };
    if doomed.is_empty() {
        return Ok(Vec::new());
    }

    let images = {
        let cipher = cipher.read();
        let read_txn = db.begin_read().map_err(store_err)?;
        let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
        let mut images = Vec::new();
        let mut retained_bytes = 0usize;
        let mut scratch = Vec::new();
        for uuid in &doomed {
            let value = clips
                .get(uuid.as_str())
                .map_err(store_err)?
                .ok_or_else(|| {
                    DomainError::Storage(format!("待清理剪贴索引指向缺失条目 {uuid}"))
                })?;
            let item = decode_record_reusing(uuid, value.value(), &cipher, &mut scratch)?;
            for path in [item.image_path, item.thumb_path].into_iter().flatten() {
                push_media_path(&mut images, &mut retained_bytes, path)?;
            }
        }
        images
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
        for uuid in &doomed {
            let value = meta.get(uuid.as_str()).map_err(store_err)?.ok_or_else(|| {
                DomainError::Storage(format!("待清理剪贴条目 {uuid} 缺少索引元数据"))
            })?;
            let (recency_key, hash) = decode_meta(uuid, value.value())?;
            let recency_key = recency_key.to_string();
            let hash = hash.to_string();
            drop(value);
            by_time.remove(recency_key.as_str()).map_err(store_err)?;
            search_filters
                .remove(recency_key.as_str())
                .map_err(store_err)?;
            remove_hash_if_owned(&mut by_hash, &hash, uuid)?;
            meta.remove(uuid.as_str()).map_err(store_err)?;
            clips.remove(uuid.as_str()).map_err(store_err)?;
        }
    }
    write_txn.commit().map_err(store_err)?;
    info!(
        operation = "clipboard_prune",
        removed = doomed.len(),
        batch_full = prune_batch_full,
        scanned,
        max_items,
        max_age_days,
        "clipboard entries pruned"
    );
    Ok(images)
}

pub(super) fn push_media_path(
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
        "剪贴媒体路径",
    )?;
    *retained_bytes = next_bytes;
    paths.push(path);
    Ok(())
}
