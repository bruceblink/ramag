use chrono::Utc;
use ramag_domain::entities::{ClipId, ClipKind};

use super::super::{encode, ensure_table, recency_key};
use super::*;

fn item(text: &str) -> ClipItem {
    ClipItem {
        id: ClipId::new(),
        kind: ClipKind::Text,
        text: Some(text.into()),
        rtf: None,
        image_path: None,
        thumb_path: None,
        image_dims: None,
        files: Vec::new(),
        preview: text.chars().take(120).collect(),
        source: None,
        byte_size: text.len() as u64,
        content_hash: "hash".into(),
        created_at: Utc::now(),
        last_used_at: Utc::now(),
    }
}

fn database_with_item(
    indexed: bool,
) -> (
    tempfile::TempDir,
    Arc<Database>,
    Arc<RwLock<Cipher>>,
    ClipItem,
) {
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::create(temp.path().join("search-index.redb")).unwrap());
    let cipher = Arc::new(RwLock::new(Cipher::new(&[7; 32])));
    let item = item("search-index-needle");
    let uuid = item.id.to_string();
    let rk = recency_key(item.last_used_at, &uuid);
    let encrypted = encode(&item, &cipher.read()).unwrap();
    let filter = build_filter(&item, &cipher.read());
    let write_txn = db.begin_write().unwrap();
    ensure_table(&write_txn).unwrap();
    {
        let mut clips = write_txn.open_table(CLIPS_TABLE).unwrap();
        let mut by_time = write_txn.open_table(CLIP_BY_TIME).unwrap();
        clips.insert(uuid.as_str(), encrypted.as_str()).unwrap();
        by_time.insert(rk.as_str(), uuid.as_str()).unwrap();
        if indexed {
            let mut filters = write_txn.open_table(CLIP_SEARCH_FILTERS).unwrap();
            filters.insert(rk.as_str(), filter.as_slice()).unwrap();
        }
    }
    if indexed {
        mark_ready(&write_txn).unwrap();
    }
    write_txn.commit().unwrap();
    (temp, db, cipher, item)
}

#[test]
fn bloom_filter_never_rejects_real_substrings() {
    let cipher = Cipher::new(&[7; 32]);
    let item = item("前缀 Mixed-CASE，换行\n以及 ÜBER 世界");
    let filter = build_filter(&item, &cipher);

    for query in ["mixed", "CASE", "über", "世界", "换行\n以"] {
        let query = query.to_lowercase();
        let query_filter = QueryFilter::new(&query, &cipher).unwrap();
        assert!(query_filter.might_match(&filter), "query={query}");
    }
}

#[test]
fn short_query_skips_bloom_filter() {
    let cipher = Cipher::new(&[7; 32]);
    assert!(QueryFilter::new("ab", &cipher).is_none());
    assert!(QueryFilter::new("界", &cipher).is_some());
}

#[test]
fn rebuild_migrates_old_records_and_preserves_search_results() {
    let (_temp, db, cipher, expected) = database_with_item(false);

    assert!(!is_ready(&db).unwrap());
    assert_eq!(rebuild_index(&db, &cipher).unwrap(), 1);
    assert!(is_ready(&db).unwrap());

    let result = search(db, cipher, "index-needle".into(), 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, expected.id);
}

#[test]
fn inconsistent_index_falls_back_without_losing_matches() {
    let (_temp, db, cipher, expected) = database_with_item(true);
    let write_txn = db.begin_write().unwrap();
    write_txn.delete_table(CLIP_SEARCH_FILTERS).unwrap();
    write_txn.open_table(CLIP_SEARCH_FILTERS).unwrap();
    write_txn.commit().unwrap();

    let result = search(db, cipher, "index-needle".into(), 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, expected.id);
}
