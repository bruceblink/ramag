//! 剪贴板全文搜索与带密钥的三字节 Bloom 预筛索引。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use rayon::prelude::*;
use redb::{Database, ReadableDatabase as _, ReadableTable, TableDefinition};

use ramag_domain::entities::{
    ClipItem, ClipSearchResult, MAX_CLIPBOARD_SEARCH_BYTES, contains_case_insensitive,
};
use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;

use super::{CLIP_BY_TIME, CLIPS_TABLE, decode_record_reusing, store_err};

pub(super) const CLIP_SEARCH_FILTERS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("clip_search_filters_v1");
pub(super) const CLIP_SEARCH_META: TableDefinition<&str, &str> =
    TableDefinition::new("clip_search_meta");

const SEARCH_INDEX_READY_KEY: &str = "ready";
const SEARCH_INDEX_VERSION: &str = "1";
const SEARCH_FILTER_BYTES: usize = 256;
const SEARCH_FILTER_BITS: usize = SEARCH_FILTER_BYTES * 8;
const SEARCH_GRAM_BYTES: usize = 3;
const SEARCH_HASHES_PER_GRAM: usize = 4;
const MAX_FILTERABLE_TEXT_BYTES: usize = 1024 * 1024;
pub(super) const PARALLEL_SEARCH_PREFIX: usize = 2_048;
const PARALLEL_SEARCH_BATCH: usize = 4_096;

#[derive(Clone, Copy)]
enum SearchProbe {
    Hit,
    Miss,
    Cancelled,
}

mod index_migration;

pub(crate) use index_migration::initialize_index;
#[cfg(test)]
use index_migration::{is_ready, rebuild_index};

struct QueryFilter {
    positions: Vec<u16>,
}

impl QueryFilter {
    fn new(query_lower: &str, cipher: &Cipher) -> Option<Self> {
        if query_lower.len() < SEARCH_GRAM_BYTES {
            return None;
        }
        let mut seen = [false; SEARCH_FILTER_BITS];
        for gram in query_lower.as_bytes().windows(SEARCH_GRAM_BYTES) {
            for position in hash_positions(cipher.search_token_hash(gram)) {
                seen[position] = true;
            }
        }
        let positions = seen
            .into_iter()
            .enumerate()
            .filter_map(|(position, present)| present.then_some(position as u16))
            .collect();
        Some(Self { positions })
    }

    fn might_match(&self, filter: &[u8]) -> bool {
        filter.len() == SEARCH_FILTER_BYTES
            && self.positions.iter().all(|position| {
                let position = usize::from(*position);
                filter[position / 8] & (1 << (position % 8)) != 0
            })
    }
}

fn hash_positions(hash: u64) -> [usize; SEARCH_HASHES_PER_GRAM] {
    [0, 16, 32, 48].map(|shift| ((hash >> shift) as usize) & (SEARCH_FILTER_BITS - 1))
}

fn insert_text(filter: &mut [u8; SEARCH_FILTER_BYTES], text: &str, cipher: &Cipher) {
    let normalized = text.to_lowercase();
    for gram in normalized.as_bytes().windows(SEARCH_GRAM_BYTES) {
        for position in hash_positions(cipher.search_token_hash(gram)) {
            filter[position / 8] |= 1 << (position % 8);
        }
    }
}

/// 超长正文直接使用全 1 过滤器，避免放大保存耗时；搜索仍会解密复核，绝不会漏结果。
pub(super) fn build_filter(item: &ClipItem, cipher: &Cipher) -> [u8; SEARCH_FILTER_BYTES] {
    let text_bytes = item.text.as_ref().map_or(0, String::len);
    if item.preview.len().saturating_add(text_bytes) > MAX_FILTERABLE_TEXT_BYTES {
        return [u8::MAX; SEARCH_FILTER_BYTES];
    }
    let mut filter = [0u8; SEARCH_FILTER_BYTES];
    insert_text(&mut filter, &item.preview, cipher);
    if let Some(text) = item.text.as_deref() {
        insert_text(&mut filter, text, cipher);
    }
    filter
}

pub(super) fn mark_ready(write_txn: &redb::WriteTransaction) -> Result<()> {
    let mut meta = write_txn.open_table(CLIP_SEARCH_META).map_err(store_err)?;
    meta.insert(SEARCH_INDEX_READY_KEY, SEARCH_INDEX_VERSION)
        .map_err(store_err)?;
    Ok(())
}

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

pub(crate) fn search_cancellable(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    query: String,
    limit: usize,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<ClipItem>> {
    Ok(search_cancellable_bounded(db, cipher, query, limit, u64::MAX, cancelled)?.items)
}

fn deep_search_pool() -> std::result::Result<&'static rayon::ThreadPool, String> {
    static POOL: OnceLock<std::result::Result<rayon::ThreadPool, String>> = OnceLock::new();
    match POOL.get_or_init(|| {
        let threads =
            std::thread::available_parallelism().map_or(2, |count| count.get().clamp(2, 8));
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("ramag-clip-search-{index}"))
            .build()
            .map_err(|error| error.to_string())
    }) {
        Ok(pool) => Ok(pool),
        Err(error) => Err(error.clone()),
    }
}

fn load_indexed_clip<T>(
    clips: &T,
    cipher: &Cipher,
    recency_key: &str,
    uuid: &str,
    scratch: &mut Vec<u8>,
) -> Result<ClipItem>
where
    T: ReadableTable<&'static str, &'static str>,
{
    let encrypted = clips.get(uuid).map_err(store_err)?.ok_or_else(|| {
        DomainError::Storage(format!("剪贴时间索引 {recency_key} 指向缺失条目 {uuid}"))
    })?;
    decode_record_reusing(uuid, encrypted.value(), cipher, scratch)
}

fn clip_matches_query(item: &ClipItem, query_lower: &str) -> bool {
    contains_case_insensitive(&item.preview, query_lower)
        || item
            .text
            .as_deref()
            .is_some_and(|text| contains_case_insensitive(text, query_lower))
}

fn append_search_hit(
    out: &mut Vec<ClipItem>,
    total_inline_bytes: &mut u64,
    item: ClipItem,
    limit: usize,
    max_inline_bytes: u64,
) -> bool {
    let next_total = total_inline_bytes.saturating_add(item.inline_payload_bytes());
    if out.len() >= limit
        || max_inline_bytes == 0
        || (!out.is_empty() && next_total > max_inline_bytes)
    {
        return true;
    }
    *total_inline_bytes = next_total;
    out.push(item);
    false
}

struct IndexedSearchRequest<'a> {
    query_lower: &'a str,
    limit: usize,
    max_inline_bytes: u64,
    cancelled: &'a AtomicBool,
}

fn indexed_search<T, F>(
    by_time: &T,
    filters: &F,
    clips: &T,
    cipher: &Cipher,
    query: &QueryFilter,
    request: &IndexedSearchRequest<'_>,
) -> Result<Option<ClipSearchResult>>
where
    T: ReadableTable<&'static str, &'static str>,
    F: ReadableTable<&'static str, &'static [u8]>,
{
    if by_time.len().map_err(store_err)? != filters.len().map_err(store_err)? {
        return Ok(None);
    }
    let mut time_entries = by_time.iter().map_err(store_err)?;
    let mut filter_entries = filters.iter().map_err(store_err)?;
    let mut out = Vec::new();
    let mut total_inline_bytes = 0u64;
    let mut truncated = false;
    let mut scratch = Vec::new();
    loop {
        if request.cancelled.load(Ordering::Relaxed) {
            break;
        }
        let pair = match (time_entries.next(), filter_entries.next()) {
            (None, None) => break,
            (Some(time), Some(filter)) => (time.map_err(store_err)?, filter.map_err(store_err)?),
            _ => return Ok(None),
        };
        let ((rk, uuid), (filter_rk, filter)) = pair;
        if rk.value() != filter_rk.value() || filter.value().len() != SEARCH_FILTER_BYTES {
            return Ok(None);
        }
        if !query.might_match(filter.value()) {
            continue;
        }
        let item = load_indexed_clip(clips, cipher, rk.value(), uuid.value(), &mut scratch)?;
        if clip_matches_query(&item, request.query_lower)
            && append_search_hit(
                &mut out,
                &mut total_inline_bytes,
                item,
                request.limit,
                request.max_inline_bytes,
            )
        {
            truncated = true;
            break;
        }
    }
    Ok(Some(ClipSearchResult {
        items: out,
        truncated,
    }))
}

fn probe_search_record<T>(
    clips: &T,
    cipher: &Cipher,
    recency_key: &str,
    uuid: &str,
    query_lower: &str,
    cancelled: &AtomicBool,
    scratch: &mut Vec<u8>,
) -> Result<SearchProbe>
where
    T: ReadableTable<&'static str, &'static str>,
{
    if cancelled.load(Ordering::Relaxed) {
        return Ok(SearchProbe::Cancelled);
    }
    let item = load_indexed_clip(clips, cipher, recency_key, uuid, scratch)?;
    Ok(if clip_matches_query(&item, query_lower) {
        SearchProbe::Hit
    } else {
        SearchProbe::Miss
    })
}

fn fallback_search<T>(
    by_time: &T,
    clips: &T,
    cipher: &Cipher,
    query_lower: &str,
    limit: usize,
    max_inline_bytes: u64,
    cancelled: &Arc<AtomicBool>,
) -> Result<ClipSearchResult>
where
    T: ReadableTable<&'static str, &'static str> + Sync,
{
    let mut out = Vec::new();
    let mut total_inline_bytes = 0u64;
    let mut truncated = false;
    let mut scratch = Vec::new();
    let mut entries = by_time.iter().map_err(store_err)?;
    'scan: {
        for _ in 0..PARALLEL_SEARCH_PREFIX {
            if cancelled.load(Ordering::Relaxed) {
                break 'scan;
            }
            let Some(entry) = entries.next() else {
                break 'scan;
            };
            let (rk, uuid) = entry.map_err(store_err)?;
            let item = load_indexed_clip(clips, cipher, rk.value(), uuid.value(), &mut scratch)?;
            if clip_matches_query(&item, query_lower)
                && append_search_hit(
                    &mut out,
                    &mut total_inline_bytes,
                    item,
                    limit,
                    max_inline_bytes,
                )
            {
                truncated = true;
                break 'scan;
            }
        }

        loop {
            if cancelled.load(Ordering::Relaxed) {
                break 'scan;
            }
            let mut batch = Vec::with_capacity(PARALLEL_SEARCH_BATCH);
            for _ in 0..PARALLEL_SEARCH_BATCH {
                let Some(entry) = entries.next() else { break };
                let (rk, uuid) = entry.map_err(store_err)?;
                batch.push((rk.value().to_string(), uuid.value().to_string()));
            }
            if batch.is_empty() {
                break 'scan;
            }

            let parallel = || {
                batch
                    .par_iter()
                    .map_init(Vec::new, |worker_scratch, (rk, uuid)| {
                        probe_search_record(
                            clips,
                            cipher,
                            rk,
                            uuid,
                            query_lower,
                            cancelled,
                            worker_scratch,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let probes = match deep_search_pool() {
                Ok(pool) => pool.install(parallel),
                Err(error) => {
                    tracing::warn!(
                        operation = "clipboard_deep_search",
                        error,
                        "clipboard deep-search pool unavailable"
                    );
                    batch
                        .iter()
                        .map(|(rk, uuid)| {
                            probe_search_record(
                                clips,
                                cipher,
                                rk,
                                uuid,
                                query_lower,
                                cancelled,
                                &mut scratch,
                            )
                        })
                        .collect()
                }
            };

            for ((rk, uuid), probe) in batch.iter().zip(probes) {
                match probe? {
                    SearchProbe::Miss => {}
                    SearchProbe::Cancelled => break 'scan,
                    SearchProbe::Hit => {
                        let item = load_indexed_clip(clips, cipher, rk, uuid, &mut scratch)?;
                        if append_search_hit(
                            &mut out,
                            &mut total_inline_bytes,
                            item,
                            limit,
                            max_inline_bytes,
                        ) {
                            truncated = true;
                            break 'scan;
                        }
                    }
                }
            }
        }
    }
    Ok(ClipSearchResult {
        items: out,
        truncated,
    })
}

pub(crate) fn search_cancellable_bounded(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    query: String,
    limit: usize,
    max_inline_bytes: u64,
    cancelled: Arc<AtomicBool>,
) -> Result<ClipSearchResult> {
    if query.len() > MAX_CLIPBOARD_SEARCH_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "剪贴历史搜索词超过 {MAX_CLIPBOARD_SEARCH_BYTES} bytes 上限"
        )));
    }
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() || cancelled.load(Ordering::Relaxed) {
        return Ok(ClipSearchResult {
            items: Vec::new(),
            truncated: false,
        });
    }
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let by_time = read_txn.open_table(CLIP_BY_TIME).map_err(store_err)?;
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;

    if let Some(query_filter) = QueryFilter::new(&query_lower, &cipher) {
        let meta = read_txn.open_table(CLIP_SEARCH_META).map_err(store_err)?;
        let ready = meta
            .get(SEARCH_INDEX_READY_KEY)
            .map_err(store_err)?
            .is_some_and(|value| value.value() == SEARCH_INDEX_VERSION);
        if ready {
            let filters = read_txn
                .open_table(CLIP_SEARCH_FILTERS)
                .map_err(store_err)?;
            let request = IndexedSearchRequest {
                query_lower: &query_lower,
                limit,
                max_inline_bytes,
                cancelled: &cancelled,
            };
            if let Some(result) =
                indexed_search(&by_time, &filters, &clips, &cipher, &query_filter, &request)?
            {
                return Ok(result);
            }
            tracing::warn!(
                operation = "clipboard_search_index",
                reason = "inconsistent",
                "clipboard search index is inconsistent; using encrypted scan"
            );
        }
    }

    fallback_search(
        &by_time,
        &clips,
        &cipher,
        &query_lower,
        limit,
        max_inline_bytes,
        &cancelled,
    )
}

#[cfg(test)]
mod tests;
