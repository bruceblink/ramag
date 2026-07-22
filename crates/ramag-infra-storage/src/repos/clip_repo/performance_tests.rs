use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use ramag_domain::entities::{ClipId, ClipItem, ClipKind, contains_case_insensitive, fnv1a_hash};
use redb::Database;

use super::*;

const SEED_BATCH: usize = 25_000;

fn perf_item(text: String, at: DateTime<Utc>) -> ClipItem {
    ClipItem {
        id: ClipId::new(),
        kind: ClipKind::Text,
        preview: text.chars().take(120).collect(),
        byte_size: text.len() as u64,
        content_hash: format!("{:016x}", fnv1a_hash(text.as_bytes())),
        text: Some(text),
        rtf: None,
        image_path: None,
        thumb_path: None,
        image_dims: None,
        files: Vec::new(),
        source: None,
        created_at: at,
        last_used_at: at,
    }
}

fn seeded_search_db(entries: usize) -> (tempfile::TempDir, Arc<Database>, Arc<RwLock<Cipher>>) {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::create(temp.path().join("clipboard-perf.redb")).unwrap();
    let write_txn = db.begin_write().unwrap();
    ensure_table(&write_txn).unwrap();
    write_txn.commit().unwrap();

    let cipher = Arc::new(RwLock::new(Cipher::new(&[7; 32])));
    let base_millis = 1_700_000_000_000i64;
    let common_text = "clipboard benchmark payload ".repeat(8);
    let common = perf_item(
        common_text,
        DateTime::from_timestamp_millis(base_millis).unwrap(),
    );
    let common_enc = encode(&common, &cipher.read()).unwrap();
    let common_filter = search::build_filter(&common, &cipher.read());
    let oldest_at = DateTime::from_timestamp_millis(base_millis - entries as i64 + 1).unwrap();
    let oldest = perf_item("needle-at-oldest".into(), oldest_at);
    let oldest_enc = encode(&oldest, &cipher.read()).unwrap();
    let oldest_filter = search::build_filter(&oldest, &cipher.read());

    for start in (0..entries).step_by(SEED_BATCH) {
        let end = (start + SEED_BATCH).min(entries);
        let write_txn = db.begin_write().unwrap();
        {
            let mut clips = write_txn.open_table(CLIPS_TABLE).unwrap();
            let mut by_time = write_txn.open_table(CLIP_BY_TIME).unwrap();
            let mut filters = write_txn.open_table(search::CLIP_SEARCH_FILTERS).unwrap();
            for index in start..end {
                let uuid = format!("{index:032x}");
                let at = DateTime::from_timestamp_millis(base_millis - index as i64).unwrap();
                let rk = recency_key(at, &uuid);
                let encrypted = if index + 1 == entries {
                    &oldest_enc
                } else {
                    &common_enc
                };
                let filter = if index + 1 == entries {
                    &oldest_filter
                } else {
                    &common_filter
                };
                clips.insert(uuid.as_str(), encrypted.as_str()).unwrap();
                by_time.insert(rk.as_str(), uuid.as_str()).unwrap();
                filters.insert(rk.as_str(), filter.as_slice()).unwrap();
            }
        }
        write_txn.commit().unwrap();
    }
    let write_txn = db.begin_write().unwrap();
    search::mark_ready(&write_txn).unwrap();
    write_txn.commit().unwrap();
    (temp, Arc::new(db), cipher)
}

fn seeded_parallel_boundary_db() -> (
    tempfile::TempDir,
    Arc<Database>,
    Arc<RwLock<Cipher>>,
    Vec<String>,
) {
    let entries = search::PARALLEL_SEARCH_PREFIX + 8;
    let temp = tempfile::tempdir().unwrap();
    let db = Database::create(temp.path().join("clipboard-boundary.redb")).unwrap();
    let write_txn = db.begin_write().unwrap();
    ensure_table(&write_txn).unwrap();
    write_txn.commit().unwrap();
    let cipher = Arc::new(RwLock::new(Cipher::new(&[9; 32])));
    let base_millis = 1_700_000_000_000i64;
    let common = perf_item(
        "ordinary clipboard content".into(),
        DateTime::from_timestamp_millis(base_millis).unwrap(),
    );
    let common_enc = encode(&common, &cipher.read()).unwrap();
    let hit_positions = [
        3,
        search::PARALLEL_SEARCH_PREFIX - 1,
        search::PARALLEL_SEARCH_PREFIX,
        entries - 1,
    ];
    let mut expected = Vec::new();

    let write_txn = db.begin_write().unwrap();
    {
        let mut clips = write_txn.open_table(CLIPS_TABLE).unwrap();
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).unwrap();
        let mut filters = write_txn.open_table(search::CLIP_SEARCH_FILTERS).unwrap();
        for index in 0..entries {
            let uuid = format!("{index:032x}");
            let at = DateTime::from_timestamp_millis(base_millis - index as i64).unwrap();
            let rk = recency_key(at, &uuid);
            if hit_positions.contains(&index) {
                let text = format!("parallel-boundary-needle-{index}");
                let item = perf_item(text.clone(), at);
                let encrypted = encode(&item, &cipher.read()).unwrap();
                let filter = search::build_filter(&item, &cipher.read());
                clips.insert(uuid.as_str(), encrypted.as_str()).unwrap();
                filters.insert(rk.as_str(), filter.as_slice()).unwrap();
                expected.push(text);
            } else {
                clips.insert(uuid.as_str(), common_enc.as_str()).unwrap();
                let filter = search::build_filter(&common, &cipher.read());
                filters.insert(rk.as_str(), filter.as_slice()).unwrap();
            }
            by_time.insert(rk.as_str(), uuid.as_str()).unwrap();
        }
    }
    search::mark_ready(&write_txn).unwrap();
    write_txn.commit().unwrap();
    (temp, Arc::new(db), cipher, expected)
}

fn median(mut samples: Vec<StdDuration>) -> StdDuration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "手动观察剪贴文本保存事务耗时"]
fn reports_clip_save_latency() {
    let iterations = std::env::var("RAMAG_PERF_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);
    assert!(iterations > 0);
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::create(temp.path().join("clipboard-save-perf.redb")).unwrap());
    let write_txn = db.begin_write().unwrap();
    ensure_table(&write_txn).unwrap();
    search::mark_ready(&write_txn).unwrap();
    write_txn.commit().unwrap();
    let cipher = Arc::new(RwLock::new(Cipher::new(&[11; 32])));
    let base_millis = 1_700_000_000_000i64;

    for index in 0..10 {
        let at = DateTime::from_timestamp_millis(base_millis + index).unwrap();
        save(
            db.clone(),
            cipher.clone(),
            perf_item(format!("warm clipboard payload {index}"), at),
        )
        .unwrap();
    }

    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let at = DateTime::from_timestamp_millis(base_millis + 10 + index as i64).unwrap();
        let item = perf_item(
            format!("clipboard save benchmark payload {index} ").repeat(8),
            at,
        );
        let started = Instant::now();
        save(db.clone(), cipher.clone(), item).unwrap();
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[((samples.len() - 1) * 95) / 100];
    eprintln!(
        "clipboard save: iterations={iterations}, p50={:.3} ms, p95={:.3} ms",
        p50.as_secs_f64() * 1_000.0,
        p95.as_secs_f64() * 1_000.0,
    );
}

#[test]
fn parallel_search_preserves_recent_order_limit_and_truncation() {
    let (_temp, db, cipher, expected) = seeded_parallel_boundary_db();
    let search = |limit| {
        search_cancellable_bounded(
            db.clone(),
            cipher.clone(),
            "boundary-needle".into(),
            limit,
            64 * 1024 * 1024,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    };

    let all = search(10);
    assert_eq!(
        all.items
            .iter()
            .map(|item| item.text.clone().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(!all.truncated);

    let limited = search(3);
    assert_eq!(
        limited
            .items
            .iter()
            .map(|item| item.text.clone().unwrap())
            .collect::<Vec<_>>(),
        expected[..3]
    );
    assert!(limited.truncated);
}

fn scan_main_table_no_match(
    db: &Arc<Database>,
    cipher: &Arc<RwLock<Cipher>>,
    query_lower: &str,
) -> Result<usize> {
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    let mut scratch = Vec::new();
    let mut hits = 0usize;
    for entry in clips.iter().map_err(store_err)? {
        let (uuid, value) = entry.map_err(store_err)?;
        let item = decode_record_reusing(uuid.value(), value.value(), &cipher, &mut scratch)?;
        if contains_case_insensitive(&item.preview, query_lower)
            || item
                .text
                .as_deref()
                .is_some_and(|text| contains_case_insensitive(text, query_lower))
        {
            hits += 1;
        }
    }
    Ok(hits)
}

fn scan_main_table_decrypt_only(db: &Arc<Database>, cipher: &Arc<RwLock<Cipher>>) -> Result<usize> {
    let cipher = cipher.read();
    let read_txn = db.begin_read().map_err(store_err)?;
    let clips = read_txn.open_table(CLIPS_TABLE).map_err(store_err)?;
    let mut scratch = Vec::new();
    let mut bytes = 0usize;
    for entry in clips.iter().map_err(store_err)? {
        let (_, value) = entry.map_err(store_err)?;
        let plaintext = cipher.decrypt_hex_into(value.value(), &mut scratch)?;
        bytes = bytes.saturating_add(black_box(plaintext.len()));
    }
    Ok(bytes)
}

#[test]
#[ignore = "手动观察大规模加密剪贴历史完整搜索耗时"]
fn reports_large_encrypted_clip_search_latency() {
    let entries = std::env::var("RAMAG_PERF_CLIP_ITEMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100_000);
    let iterations = std::env::var("RAMAG_PERF_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(3);
    assert!(entries > 0);
    assert!(iterations > 0);
    let (_temp, db, cipher) = seeded_search_db(entries);
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let result = search_cancellable_bounded(
            db.clone(),
            cipher.clone(),
            "needle-not-present".into(),
            500,
            64 * 1024 * 1024,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        black_box(result);
        samples.push(started.elapsed());
    }
    let time_index_median = median(samples);
    let mut recent_samples = Vec::with_capacity(iterations);
    let mut oldest_samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let recent = search_cancellable_bounded(
            db.clone(),
            cipher.clone(),
            "clipboard".into(),
            500,
            64 * 1024 * 1024,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(black_box(recent.items.len()), 500);
        assert!(recent.truncated);
        recent_samples.push(started.elapsed());

        let started = Instant::now();
        let oldest = search_cancellable_bounded(
            db.clone(),
            cipher.clone(),
            "needle-at-oldest".into(),
            500,
            64 * 1024 * 1024,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(black_box(oldest.items.len()), 1);
        oldest_samples.push(started.elapsed());
    }
    let recent_median = median(recent_samples);
    let oldest_median = median(oldest_samples);
    let mut sequential_samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        assert_eq!(
            black_box(scan_main_table_no_match(&db, &cipher, "needle-not-present")).unwrap(),
            0
        );
        sequential_samples.push(started.elapsed());
    }
    let sequential_median = median(sequential_samples);
    let started = Instant::now();
    assert!(black_box(scan_main_table_decrypt_only(&db, &cipher)).unwrap() > 0);
    let decrypt_only = started.elapsed();
    eprintln!(
        "clipboard encrypted search: items={entries}, iterations={iterations}, no_match={:.3} ms, oldest_hit={:.3} ms, recent_500={:.3} ms, sequential_main={:.3} ms, decrypt_only={:.3} ms",
        time_index_median.as_secs_f64() * 1_000.0,
        oldest_median.as_secs_f64() * 1_000.0,
        recent_median.as_secs_f64() * 1_000.0,
        sequential_median.as_secs_f64() * 1_000.0,
        decrypt_only.as_secs_f64() * 1_000.0,
    );
}
