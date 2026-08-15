use std::ops::Bound;
use std::sync::Arc;

use parking_lot::RwLock;
use redb::{Database, ReadableDatabase as _, ReadableTable as _, ReadableTableMetadata as _};

use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;

use super::{
    CLIP_SEARCH_FILTERS, CLIP_SEARCH_META, SEARCH_INDEX_READY_KEY, SEARCH_INDEX_VERSION,
    build_filter, mark_ready,
};
use crate::repos::clip_repo::{CLIP_BY_TIME, CLIPS_TABLE, decode_record_reusing, store_err};

const SEARCH_INDEX_MIGRATION_BATCH: usize = 10_000;

pub(super) fn is_ready(db: &Database) -> Result<bool> {
    let read_txn = db.begin_read().map_err(store_err)?;
    let meta = read_txn.open_table(CLIP_SEARCH_META).map_err(store_err)?;
    Ok(meta
        .get(SEARCH_INDEX_READY_KEY)
        .map_err(store_err)?
        .is_some_and(|value| value.value() == SEARCH_INDEX_VERSION))
}

/// 为旧数据库后台补建搜索索引。
pub(crate) fn initialize_index(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>) -> Result<()> {
    if is_ready(&db)? {
        return Ok(());
    }
    let is_empty = {
        let read_txn = db.begin_read().map_err(store_err)?;
        let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        by_time.is_empty().map_err(store_err)?
    };
    if is_empty {
        let write_txn = db.begin_write().map_err(store_err)?;
        mark_ready(&write_txn)?;
        write_txn.commit().map_err(store_err)?;
        return Ok(());
    }

    std::thread::Builder::new()
        .name("ramag-clip-search-index".into())
        .spawn(move || match rebuild_index(&db, &cipher) {
            Ok(count) => tracing::info!(
                operation = "clipboard_search_index_migrate",
                count,
                "clipboard search index migrated"
            ),
            Err(error) => tracing::warn!(
                operation = "clipboard_search_index_migrate",
                error = %error,
                "clipboard search index migration failed"
            ),
        })
        .map_err(|error| DomainError::Storage(format!("启动剪贴搜索索引迁移失败：{error}")))?;
    Ok(())
}

pub(super) fn rebuild_index(db: &Database, cipher: &RwLock<Cipher>) -> Result<usize> {
    let reset_txn = db.begin_write().map_err(store_err)?;
    reset_txn
        .delete_table(CLIP_SEARCH_FILTERS)
        .map_err(store_err)?;
    reset_txn
        .open_table(CLIP_SEARCH_FILTERS)
        .map_err(store_err)?;
    {
        let mut meta = reset_txn.open_table(CLIP_SEARCH_META).map_err(store_err)?;
        meta.remove(SEARCH_INDEX_READY_KEY).map_err(store_err)?;
    }
    reset_txn.commit().map_err(store_err)?;

    let cipher = cipher.read();
    let mut last_key: Option<String> = None;
    let mut migrated = 0usize;
    loop {
        let batch = {
            let read_txn = db.begin_read().map_err(store_err)?;
            let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
            let lower = last_key
                .as_deref()
                .map_or(Bound::Unbounded, Bound::Excluded);
            let mut batch = Vec::with_capacity(SEARCH_INDEX_MIGRATION_BATCH);
            for entry in by_time
                .range::<&str>((lower, Bound::Unbounded))
                .map_err(store_err)?
                .take(SEARCH_INDEX_MIGRATION_BATCH)
            {
                let (recency_key, uuid) = entry.map_err(store_err)?;
                batch.push((recency_key.value().to_string(), uuid.value().to_string()));
            }
            batch
        };
        let Some((next_last, _)) = batch.last() else {
            break;
        };
        last_key = Some(next_last.clone());

        let write_txn = db.begin_write().map_err(store_err)?;
        {
            let by_time = write_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
            let clips = write_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
            let mut filters = write_txn
                .open_table(CLIP_SEARCH_FILTERS)
                .map_err(store_err)?;
            let mut scratch = Vec::new();
            for (recency_key, uuid) in &batch {
                let current = by_time.get(recency_key.as_str()).map_err(store_err)?;
                if current.is_none_or(|value| value.value() != uuid) {
                    continue;
                }
                let encrypted = clips
                    .get(uuid.as_str())
                    .map_err(store_err)?
                    .ok_or_else(|| {
                        DomainError::Storage(format!(
                            "剪贴时间索引 {recency_key} 指向缺失条目 {uuid}"
                        ))
                    })?;
                let item = decode_record_reusing(uuid, encrypted.value(), &cipher, &mut scratch)?;
                let filter = build_filter(&item, &cipher);
                filters
                    .insert(recency_key.as_str(), filter.as_slice())
                    .map_err(store_err)?;
                migrated = migrated.saturating_add(1);
            }
        }
        write_txn.commit().map_err(store_err)?;
    }

    let finish_txn = db.begin_write().map_err(store_err)?;
    {
        let by_time = finish_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
        let filters = finish_txn
            .open_table(CLIP_SEARCH_FILTERS)
            .map_err(store_err)?;
        let expected = by_time.len().map_err(store_err)?;
        let actual = filters.len().map_err(store_err)?;
        if actual != expected {
            return Err(DomainError::Storage(format!(
                "剪贴搜索索引迁移数量不一致：{actual} / {expected}"
            )));
        }
    }
    mark_ready(&finish_txn)?;
    finish_txn.commit().map_err(store_err)?;
    Ok(migrated)
}
