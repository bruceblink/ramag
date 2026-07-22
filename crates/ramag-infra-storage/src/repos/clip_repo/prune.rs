//! 剪贴历史超量与超龄清理候选选择。

use redb::ReadableTable;

use ramag_domain::error::Result;

use super::{MAX_CLIP_PRUNE_BATCH, millis_from_recency_key, store_err};

pub(super) struct PruneSelection {
    pub(super) doomed: Vec<String>,
    pub(super) batch_full: bool,
    pub(super) scanned: usize,
}

pub(super) fn select_prune_candidates<T>(
    by_time: &T,
    excess: u64,
    cutoff_millis: i64,
) -> Result<PruneSelection>
where
    T: ReadableTable<&'static str, &'static str>,
{
    let initial_capacity = excess.min(MAX_CLIP_PRUNE_BATCH as u64).max(1) as usize;
    let mut doomed = Vec::with_capacity(initial_capacity);
    let mut scanned = 0usize;
    // 时间索引按最新到最旧排列。反向扫描后，超量清理只访问实际需要删除的最旧项；
    // 一旦不再超量且未过期，后续记录只会更新，可立即停止。
    for (oldest_index, entry) in by_time.iter().map_err(store_err)?.rev().enumerate() {
        scanned = scanned.saturating_add(1);
        let (rk, uuid) = entry.map_err(store_err)?;
        let over_count = (oldest_index as u64) < excess;
        let over_age = millis_from_recency_key(rk.value())? < cutoff_millis;
        if !over_count && !over_age {
            break;
        }
        doomed.push(uuid.value().to_string());
        if doomed.len() >= MAX_CLIP_PRUNE_BATCH {
            break;
        }
    }
    Ok(PruneSelection {
        batch_full: doomed.len() >= MAX_CLIP_PRUNE_BATCH,
        doomed,
        scanned,
    })
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use redb::{Database, ReadableDatabase as _};

    use super::*;
    use crate::repos::clip_repo::{CLIP_BY_TIME, recency_key};

    fn indexed_db(entries: usize) -> (tempfile::TempDir, Database) {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::create(temp.path().join("clips.redb")).unwrap();
        let write_txn = db.begin_write().unwrap();
        {
            let mut by_time = write_txn.open_table(CLIP_BY_TIME).unwrap();
            let base_millis = 1_700_000_000_000i64;
            for index in 0..entries {
                let uuid = format!("{index:032x}");
                let at = DateTime::from_timestamp_millis(base_millis - index as i64).unwrap();
                let rk = recency_key(at, &uuid);
                by_time.insert(rk.as_str(), uuid.as_str()).unwrap();
            }
        }
        write_txn.commit().unwrap();
        (temp, db)
    }

    #[test]
    fn selection_visits_only_oldest_boundary_for_count_limit() {
        let (_temp, db) = indexed_db(20_000);
        let read_txn = db.begin_read().unwrap();
        let by_time = read_txn.open_table(CLIP_BY_TIME).unwrap();

        let selection = select_prune_candidates(&by_time, 1, 0).unwrap();

        assert_eq!(selection.doomed, vec![format!("{:032x}", 19_999)]);
        assert_eq!(selection.scanned, 2);
        assert!(!selection.batch_full);
    }

    #[test]
    fn selection_unifies_count_and_age_without_scanning_newer_prefix() {
        let (_temp, db) = indexed_db(10);
        let read_txn = db.begin_read().unwrap();
        let by_time = read_txn.open_table(CLIP_BY_TIME).unwrap();
        let cutoff_millis = 1_700_000_000_000i64 - 5;

        let selection = select_prune_candidates(&by_time, 2, cutoff_millis).unwrap();

        let expected = (6..10)
            .rev()
            .map(|index| format!("{index:032x}"))
            .collect::<Vec<_>>();
        assert_eq!(selection.doomed, expected);
        assert_eq!(selection.scanned, 5);
        assert!(!selection.batch_full);
    }

    #[test]
    #[ignore = "手动观察百万历史超限清理候选定位耗时"]
    fn reports_large_selection_latency() {
        use std::hint::black_box;
        use std::time::Instant;

        let entries = std::env::var("RAMAG_PERF_CLIP_ITEMS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_000_000);
        assert!(entries >= 2);
        let (_temp, db) = indexed_db(entries);
        let read_txn = db.begin_read().unwrap();
        let by_time = read_txn.open_table(CLIP_BY_TIME).unwrap();

        let started = Instant::now();
        let selection = black_box(select_prune_candidates(&by_time, 1, 0).unwrap());
        let optimized = started.elapsed();
        assert_eq!(selection.doomed.len(), 1);
        assert_eq!(selection.scanned, 2);

        // 保留旧方向作为只读参照：从最新端定位一个超限项必须跳过整个保留窗口。
        let started = Instant::now();
        let mut reference_scanned = 0usize;
        let mut reference_doomed = 0usize;
        for (index, entry) in by_time.iter().unwrap().enumerate() {
            reference_scanned += 1;
            let (rk, _) = entry.unwrap();
            let over_count = index >= entries - 1;
            let over_age = millis_from_recency_key(rk.value()).unwrap() < 0;
            if over_count || over_age {
                reference_doomed += 1;
                break;
            }
        }
        let forward_reference = started.elapsed();
        assert_eq!(reference_doomed, 1);
        assert_eq!(reference_scanned, entries);

        eprintln!(
            "clipboard prune selection: items={entries}, reverse_scanned={}, reverse={:.3} us, forward_scanned={reference_scanned}, forward={:.3} ms",
            selection.scanned,
            optimized.as_secs_f64() * 1_000_000.0,
            forward_reference.as_secs_f64() * 1_000.0,
        );
    }
}
