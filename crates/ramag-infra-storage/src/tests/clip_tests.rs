use super::*;

fn sample_clip(text: &str, age_days: i64) -> ramag_domain::entities::ClipItem {
    let at = Utc::now() - Duration::days(age_days);
    ramag_domain::entities::ClipItem {
        id: ClipId::new(),
        kind: ClipKind::Text,
        text: Some(text.to_string()),
        rtf: None,
        image_path: None,
        thumb_path: None,
        image_dims: None,
        files: Vec::new(),
        preview: text.to_string(),
        source: None,
        byte_size: text.len() as u64,
        content_hash: format!(
            "{:016x}",
            ramag_domain::entities::fnv1a_hash(text.as_bytes())
        ),
        created_at: at,
        last_used_at: at,
    }
}

#[tokio::test]
async fn clip_save_list_roundtrip_sorted() {
    let (storage, _tmp) = make_test_storage();
    storage.clip_save(&sample_clip("old", 3)).await.unwrap();
    storage.clip_save(&sample_clip("new", 0)).await.unwrap();

    let list = storage.clip_list().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].text.as_deref(), Some("new"));
    assert_eq!(list[1].text.as_deref(), Some("old"));
}

#[tokio::test]
async fn clip_find_by_hash_and_delete() {
    let (storage, _tmp) = make_test_storage();
    let clip = sample_clip("dup-me", 0);
    storage.clip_save(&clip).await.unwrap();

    assert_eq!(
        storage
            .clip_get(&clip.id)
            .await
            .unwrap()
            .map(|item| item.id),
        Some(clip.id.clone())
    );
    let found = storage.clip_find_by_hash(&clip.content_hash).await.unwrap();
    assert_eq!(found.unwrap().id, clip.id);
    assert!(storage.clip_find_by_hash("ffff").await.unwrap().is_none());

    storage.clip_delete(&clip.id).await.unwrap();
    assert!(storage.clip_get(&clip.id).await.unwrap().is_none());
    assert!(storage.clip_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn clip_clear_removes_all() {
    let (storage, _tmp) = make_test_storage();
    storage.clip_save(&sample_clip("a", 0)).await.unwrap();
    storage.clip_save(&sample_clip("b", 0)).await.unwrap();

    storage.clip_clear().await.unwrap();
    assert!(storage.clip_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn clip_clear_recovers_from_corrupted_record() {
    let (storage, _tmp) = make_test_storage();
    let clip = sample_clip("corrupted", 0);
    storage.clip_save(&clip).await.unwrap();
    {
        let txn = storage.db.begin_write().unwrap();
        {
            let mut clips = txn.open_table(repos::clip_repo::CLIPS_TABLE).unwrap();
            clips
                .insert(clip.id.to_string().as_str(), "not-valid-ciphertext")
                .unwrap();
        }
        txn.commit().unwrap();
    }

    assert!(storage.clip_clear().await.is_ok());
    assert!(storage.clip_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn clip_media_paths_returns_only_referenced_files() {
    let (storage, _tmp) = make_test_storage();
    storage.clip_save(&sample_clip("text", 0)).await.unwrap();
    let mut image = sample_clip("image", 0);
    image.kind = ClipKind::Image;
    image.text = None;
    image.image_path = Some("full.img".into());
    image.thumb_path = Some("thumb.img".into());
    storage.clip_save(&image).await.unwrap();

    let mut paths = storage.clip_media_paths().await.unwrap();
    paths.sort();
    assert_eq!(paths, vec!["full.img", "thumb.img"]);
}

#[tokio::test]
async fn clip_prune_by_count_and_age() {
    let (storage, _tmp) = make_test_storage();
    storage
        .clip_save(&sample_clip("expired", 40))
        .await
        .unwrap();
    storage.clip_save(&sample_clip("kept-1", 1)).await.unwrap();
    storage.clip_save(&sample_clip("kept-2", 0)).await.unwrap();

    // 数量上限 5、保留 30 天：仅超龄 expired 被剔
    storage.clip_prune(5, 30).await.unwrap();
    let rest = storage.clip_list().await.unwrap();
    let texts: Vec<_> = rest.iter().map(|c| c.text.clone().unwrap()).collect();
    assert_eq!(rest.len(), 2);
    assert!(texts.contains(&"kept-1".to_string()));
    assert!(texts.contains(&"kept-2".to_string()));

    // 数量上限 1：只留最新
    storage.clip_prune(1, 30).await.unwrap();
    let rest = storage.clip_list().await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].text.as_deref(), Some("kept-2"));
}

#[tokio::test]
async fn clip_list_recent_order_and_limit() {
    let (storage, _tmp) = make_test_storage();
    storage.clip_save(&sample_clip("oldest", 3)).await.unwrap();
    storage.clip_save(&sample_clip("mid", 2)).await.unwrap();
    storage.clip_save(&sample_clip("newest", 0)).await.unwrap();

    // limit 截断 + 最近优先
    let recent = storage.clip_list_recent(2).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].text.as_deref(), Some("newest"));
    assert_eq!(recent[1].text.as_deref(), Some("mid"));

    // limit 超总数 → 全部返回
    let all = storage.clip_list_recent(100).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn clip_list_recent_respects_inline_byte_budget() {
    let (storage, _tmp) = make_test_storage();
    storage.clip_save(&sample_clip("old", 3)).await.unwrap();
    storage.clip_save(&sample_clip("mid", 2)).await.unwrap();
    storage.clip_save(&sample_clip("newest", 0)).await.unwrap();

    let one = storage.clip_list_recent_bounded(10, 5).await.unwrap();
    assert_eq!(one.len(), 1, "最新一条自身超限时仍应可见");
    assert_eq!(one[0].text.as_deref(), Some("newest"));

    let two = storage.clip_list_recent_bounded(10, 9).await.unwrap();
    assert_eq!(two.len(), 2);
    assert_eq!(two[1].text.as_deref(), Some("mid"));
}

#[tokio::test]
async fn clip_update_refreshes_recency_without_dup() {
    let (storage, _tmp) = make_test_storage();
    let mut a = sample_clip("a", 5);
    let b = sample_clip("b", 0);
    storage.clip_save(&a).await.unwrap();
    storage.clip_save(&b).await.unwrap();
    assert_eq!(
        storage.clip_list_recent(10).await.unwrap()[0]
            .text
            .as_deref(),
        Some("b")
    );

    // 提升 a（同 id 更新 last_used）→ 旧时间索引项须清除，不得产生重复
    a.last_used_at = Utc::now();
    storage.clip_save(&a).await.unwrap();
    let r = storage.clip_list_recent(10).await.unwrap();
    assert_eq!(r.len(), 2, "更新不应产生重复条目");
    assert_eq!(r[0].text.as_deref(), Some("a"));
    assert_eq!(r[1].text.as_deref(), Some("b"));
}

#[tokio::test]
async fn clip_update_removes_stale_hash_mapping() {
    let (storage, _tmp) = make_test_storage();
    let mut clip = sample_clip("hash-change", 0);
    let old_hash = clip.content_hash.clone();
    storage.clip_save(&clip).await.unwrap();

    clip.content_hash = "replacement-hash".into();
    storage.clip_save(&clip).await.unwrap();

    assert!(
        storage
            .clip_find_by_hash(&old_hash)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        storage
            .clip_find_by_hash(&clip.content_hash)
            .await
            .unwrap()
            .unwrap()
            .id,
        clip.id
    );
}

#[tokio::test]
async fn deleting_hash_collision_does_not_remove_other_mapping() {
    let (storage, _tmp) = make_test_storage();
    let first = sample_clip("first", 1);
    let mut second = sample_clip("second", 0);
    second.content_hash = first.content_hash.clone();
    storage.clip_save(&first).await.unwrap();
    storage.clip_save(&second).await.unwrap();

    storage.clip_delete(&first.id).await.unwrap();

    assert_eq!(
        storage
            .clip_find_by_hash(&first.content_hash)
            .await
            .unwrap()
            .unwrap()
            .id,
        second.id
    );
}

#[tokio::test]
async fn dangling_clip_time_index_is_reported() {
    let (storage, _tmp) = make_test_storage();
    let clip = sample_clip("dangling", 0);
    storage.clip_save(&clip).await.unwrap();
    {
        let txn = storage.db.begin_write().unwrap();
        {
            let mut clips = txn.open_table(repos::clip_repo::CLIPS_TABLE).unwrap();
            clips.remove(clip.id.to_string().as_str()).unwrap();
        }
        txn.commit().unwrap();
    }

    let error = storage.clip_list_recent(10).await.unwrap_err();
    assert!(error.to_string().contains("指向缺失条目"));
}

#[tokio::test]
async fn corrupted_clip_meta_blocks_unsafe_update() {
    let (storage, _tmp) = make_test_storage();
    let mut clip = sample_clip("corrupt-meta", 1);
    storage.clip_save(&clip).await.unwrap();
    {
        let txn = storage.db.begin_write().unwrap();
        {
            let mut meta = txn.open_table(repos::clip_repo::CLIP_UUID_META).unwrap();
            meta.insert(clip.id.to_string().as_str(), "invalid-meta")
                .unwrap();
        }
        txn.commit().unwrap();
    }

    clip.last_used_at = Utc::now();
    let error = storage.clip_save(&clip).await.unwrap_err();
    assert!(error.to_string().contains("索引元数据"));
}

#[tokio::test]
async fn clip_migrate_rebuilds_indexes_from_main_table() {
    let (storage, _tmp) = make_test_storage();
    let c1 = sample_clip("alpha", 2);
    let c2 = sample_clip("beta", 0);
    storage.clip_save(&c1).await.unwrap();
    storage.clip_save(&c2).await.unwrap();

    // 模拟索引丢失：删时间索引表后重建空表（主表保留），mirror open 时 ensure→migrate 流程
    {
        let txn = storage.db.begin_write().unwrap();
        txn.delete_table(repos::clip_repo::CLIP_BY_TIME).unwrap();
        repos::clip_repo::ensure_table(&txn).unwrap();
        txn.commit().unwrap();
    }
    repos::clip_repo::migrate_indexes(storage.db.clone(), storage.cipher.clone()).unwrap();

    let recent = storage.clip_list_recent(10).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].text.as_deref(), Some("beta"));
    assert!(
        storage
            .clip_find_by_hash(&c1.content_hash)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn clip_search_matches_recent_first_and_limit() {
    let (storage, _tmp) = make_test_storage();
    storage
        .clip_save(&sample_clip("hello world", 2))
        .await
        .unwrap();
    storage.clip_save(&sample_clip("foo bar", 1)).await.unwrap();
    storage
        .clip_save(&sample_clip("hello rust", 0))
        .await
        .unwrap();

    // 匹配 + 最近优先
    let r = storage.clip_search("hello", 10).await.unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].text.as_deref(), Some("hello rust"));
    assert_eq!(r[1].text.as_deref(), Some("hello world"));

    // ASCII 大小写不敏感匹配走无分配路径
    assert_eq!(storage.clip_search("HELLO", 10).await.unwrap().len(), 2);

    // limit 早停
    let r = storage.clip_search("hello", 1).await.unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].text.as_deref(), Some("hello rust"));

    // 空 query / 无匹配 → 空
    assert!(storage.clip_search("", 10).await.unwrap().is_empty());
    assert!(storage.clip_search("zzz", 10).await.unwrap().is_empty());
    assert!(
        storage
            .clip_search(&"x".repeat(MAX_CLIPBOARD_SEARCH_BYTES + 1), 10)
            .await
            .is_err()
    );

    let bounded = storage
        .clip_search_cancellable_bounded("hello", 10, 10, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(bounded.items.len(), 1);
    assert!(bounded.truncated);

    // 已取消的搜索不得继续扫描历史
    let cancelled = Arc::new(AtomicBool::new(true));
    assert!(
        storage
            .clip_search_cancellable("hello", 10, cancelled)
            .await
            .unwrap()
            .is_empty()
    );
}
