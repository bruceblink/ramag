use super::*;
use ramag_domain::entities::MAX_CLIPBOARD_ITEM_BYTES;

fn settings() -> ClipboardSettings {
    ClipboardSettings::default()
}

fn text_clip(s: &str) -> CapturedClip {
    CapturedClip {
        text: Some(s.to_string()),
        ..Default::default()
    }
}

#[test]
fn concealed_is_skipped() {
    let c = CapturedClip {
        concealed: true,
        ..Default::default()
    };
    assert_eq!(
        decide_capture(&c, &settings()),
        CaptureDecision::Skip("concealed")
    );
}

#[test]
fn settings_parser_rejects_oversized_or_unbounded_payloads() {
    let oversized = " ".repeat(MAX_SETTINGS_JSON_BYTES + 1);
    assert!(parse_clipboard_settings(&oversized).is_err());

    let settings = ClipboardSettings {
        max_item_bytes: MAX_CLIPBOARD_ITEM_BYTES + 1,
        ..ClipboardSettings::default()
    };
    let json = serde_json::to_string(&settings);
    assert!(json.is_ok_and(|json| parse_clipboard_settings(&json).is_err()));
    assert!(serialize_clipboard_settings(&settings).is_err());
}

#[test]
fn empty_and_oversize_text_skipped() {
    assert_eq!(
        decide_capture(&text_clip("   "), &settings()),
        CaptureDecision::Skip("empty text")
    );
    let mut s = settings();
    s.max_item_bytes = 4;
    assert_eq!(
        decide_capture(&text_clip("toolong"), &s),
        CaptureDecision::Skip("text too large")
    );
}

#[test]
fn text_classified_and_hashed() {
    let d = decide_capture(&text_clip("https://example.com/x"), &settings());
    match d {
        CaptureDecision::Record { kind, hash } => {
            assert_eq!(kind, ClipKind::Link);
            assert_eq!(hash.len(), 16);
        }
        _ => panic!("应记录"),
    }
}

#[test]
fn files_take_priority_over_text() {
    let c = CapturedClip {
        text: Some("/path/as/text".into()),
        files: vec!["/path/a".into(), "/path/b".into()],
        ..Default::default()
    };
    assert!(matches!(
        decide_capture(&c, &settings()),
        CaptureDecision::Record {
            kind: ClipKind::Files,
            ..
        }
    ));

    let mut limited = settings();
    limited.max_item_bytes = 3;
    assert_eq!(
        decide_capture(&c, &limited),
        CaptureDecision::Skip("files too large")
    );
}

#[test]
fn file_payload_helpers_preserve_joined_representation() {
    let files = vec!["/tmp/a".to_string(), "目录/文件".to_string(), String::new()];
    let joined = files.join("\n");

    assert_eq!(file_payload_len(&files), joined.len() as u64);
    assert_eq!(file_payload_hash(&files), fnv1a_hash(joined.as_bytes()));
    assert_eq!(
        reverse_file_payload_hash(&files),
        reverse_fnv1a_hash(joined.as_bytes())
    );
}

#[test]
fn cache_keeps_recent_contiguous_prefix_within_byte_budget() {
    let make_item = |text: &str| {
        Arc::new(ClipItem {
            id: ClipId::new(),
            kind: ClipKind::Text,
            text: Some(text.into()),
            rtf: None,
            image_path: None,
            thumb_path: None,
            image_dims: None,
            files: Vec::new(),
            preview: text.into(),
            source: None,
            byte_size: text.len() as u64,
            content_hash: hash_hex(text.as_bytes()),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
        })
    };
    let mut cache = vec![make_item("newest"), make_item("mid"), make_item("old")];

    truncate_cache(&mut cache, 500, 9);

    assert_eq!(cache.len(), 2);
    assert_eq!(cache[0].text.as_deref(), Some("newest"));
    assert_eq!(cache[1].text.as_deref(), Some("mid"));
}

#[test]
fn rich_text_counts_towards_size_limit() {
    let clip = CapturedClip {
        text: Some("a".into()),
        rtf: Some(vec![0; 8]),
        ..Default::default()
    };
    let mut limited = settings();
    limited.max_item_bytes = 8;
    assert_eq!(
        decide_capture(&clip, &limited),
        CaptureDecision::Skip("text too large")
    );
}

#[test]
fn image_respects_size_and_toggle() {
    let big = CapturedClip {
        image_png: Some(vec![0u8; 100]),
        image_dims: Some((10, 10)),
        ..Default::default()
    };
    let mut s = settings();
    s.max_item_bytes = 50;
    assert_eq!(
        decide_capture(&big, &s),
        CaptureDecision::Skip("image too large")
    );

    let small = CapturedClip {
        image_png: Some(vec![0u8; 10]),
        image_dims: Some((2, 2)),
        ..Default::default()
    };
    let mut s2 = settings();
    s2.capture_images = false;
    assert_eq!(
        decide_capture(&small, &s2),
        CaptureDecision::Skip("image capture disabled")
    );
    assert!(matches!(
        decide_capture(&small, &settings()),
        CaptureDecision::Record {
            kind: ClipKind::Image,
            ..
        }
    ));

    let missing_dims = CapturedClip {
        image_png: Some(vec![0u8; 10]),
        ..Default::default()
    };
    assert_eq!(
        decide_capture(&missing_dims, &settings()),
        CaptureDecision::Skip("invalid image")
    );

    let oversized_dims = CapturedClip {
        image_png: Some(vec![0u8; 10]),
        image_dims: Some((16_385, 1)),
        ..Default::default()
    };
    assert_eq!(
        decide_capture(&oversized_dims, &settings()),
        CaptureDecision::Skip("image dimensions too large")
    );
}

#[test]
fn pending_media_delete_can_restore_or_expire_once() {
    let pending = PendingMediaDeletes::default();
    let restored_id = ClipId::new();
    let first_token = pending.stage(restored_id.clone(), vec!["a.img".into(), "a.thumb".into()]);
    assert_eq!(
        pending.take_for_restore(&restored_id),
        Some((first_token, vec!["a.img".into(), "a.thumb".into()]))
    );
    assert_eq!(pending.expire(&restored_id, first_token), None);

    // 同一个条目撤销后再次删除：第一次删除的旧计时器不得清掉第二次删除的媒体。
    let second_token = pending.stage(restored_id.clone(), vec!["b.img".into()]);
    assert_eq!(pending.expire(&restored_id, first_token), None);
    assert_eq!(
        pending.expire(&restored_id, second_token),
        Some(vec!["b.img".into()])
    );
    assert_eq!(pending.take_for_restore(&restored_id), None);
}

#[test]
fn failed_restore_keeps_original_cleanup_token_valid() {
    let pending = PendingMediaDeletes::default();
    let id = ClipId::new();
    let token = pending.stage(id.clone(), vec!["image.img".into()]);
    let (taken_token, paths) = pending.take_for_restore(&id).unwrap_or((0, Vec::new()));

    pending.put_back(id.clone(), taken_token, paths);

    assert_eq!(taken_token, token);
    assert_eq!(pending.expire(&id, token), Some(vec!["image.img".into()]));
}

#[test]
fn clearing_history_invalidates_pending_media_restores() {
    let pending = PendingMediaDeletes::default();
    let id = ClipId::new();
    let token = pending.stage(id.clone(), vec!["image.img".into()]);

    pending.clear();

    assert_eq!(pending.take_for_restore(&id), None);
    assert_eq!(pending.expire(&id, token), None);
}

#[test]
fn reused_media_is_removed_from_old_physical_delete_timer() {
    let pending = PendingMediaDeletes::default();
    let id = ClipId::new();
    let token = pending.stage(
        id.clone(),
        vec!["shared.img".into(), "old-only.thumb".into()],
    );

    assert!(pending.contains_path("shared.img"));
    pending.protect_paths(["shared.img"]);

    assert!(!pending.contains_path("shared.img"));
    assert_eq!(
        pending.expire(&id, token),
        Some(vec!["old-only.thumb".into()])
    );
}

#[test]
fn touching_item_preserves_rich_text_payload() {
    let created_at = Utc::now() - chrono::Duration::minutes(1);
    let item = ClipItem {
        id: ClipId::new(),
        kind: ClipKind::Text,
        text: Some("rich text".into()),
        rtf: Some(b"{\\rtf1 rich text}".to_vec()),
        image_path: None,
        thumb_path: None,
        image_dims: None,
        files: Vec::new(),
        preview: "rich text".into(),
        source: None,
        byte_size: 24,
        content_hash: "hash".into(),
        created_at,
        last_used_at: created_at,
    };
    let now = Utc::now();

    let touched = touch_item(&item, now);

    assert_eq!(touched.last_used_at, now);
    assert_eq!(touched.created_at, item.created_at);
    assert_eq!(touched.text, item.text);
    assert_eq!(touched.rtf, item.rtf);
    assert_eq!(touched.content_hash, item.content_hash);
}

#[test]
fn collision_hash_is_stable_and_separate_from_primary_hash() {
    let clip = text_clip("collision payload");
    let text = clip.text.as_deref().unwrap_or_default();
    let primary = hash_hex(text.as_bytes());

    let first = collision_hash(&clip, &primary);
    let second = collision_hash(&clip, &primary);

    assert_eq!(first, second);
    assert!(first.starts_with(&format!("{primary}-")));
    assert_ne!(first, primary);
}

#[test]
fn inline_payload_match_rejects_same_hash_with_different_content() {
    let existing = ClipItem {
        id: ClipId::new(),
        kind: ClipKind::Text,
        text: Some("first".into()),
        rtf: None,
        image_path: None,
        thumb_path: None,
        image_dims: None,
        files: Vec::new(),
        preview: "first".into(),
        source: None,
        byte_size: 5,
        content_hash: "same-hash".into(),
        created_at: Utc::now(),
        last_used_at: Utc::now(),
    };

    assert!(inline_payload_matches(
        &existing,
        &text_clip("first"),
        ClipKind::Text
    ));
    assert!(!inline_payload_matches(
        &existing,
        &text_clip("second"),
        ClipKind::Text
    ));
}

#[test]
fn restore_dedup_only_merges_the_same_payload() {
    let first = ClipItem {
        id: ClipId::new(),
        kind: ClipKind::Text,
        text: Some("same".into()),
        rtf: None,
        image_path: None,
        thumb_path: None,
        image_dims: None,
        files: Vec::new(),
        preview: "same".into(),
        source: None,
        byte_size: 4,
        content_hash: "same-hash".into(),
        created_at: Utc::now(),
        last_used_at: Utc::now(),
    };
    let mut duplicate = first.clone();
    duplicate.id = ClipId::new();
    assert!(super::media_ops::clip_items_share_payload(
        &first, &duplicate
    ));

    duplicate.text = Some("collision".into());
    assert!(!super::media_ops::clip_items_share_payload(
        &first, &duplicate
    ));
}

#[test]
fn clipboard_search_query_has_explicit_resource_boundary() {
    assert!(validate_search_query(&"x".repeat(MAX_CLIPBOARD_SEARCH_BYTES)).is_ok());
    assert!(validate_search_query(&"x".repeat(MAX_CLIPBOARD_SEARCH_BYTES + 1)).is_err());
}
