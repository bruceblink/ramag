use super::*;

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
        decide_capture(&c, &settings(), None),
        CaptureDecision::Skip("concealed")
    );
}

#[test]
fn blacklist_skips_by_bundle() {
    let mut s = settings();
    s.blacklist.push("com.1password.1password".into());
    let src = ClipSource {
        bundle_id: "com.1password.1password".into(),
        name: "1Password".into(),
    };
    assert_eq!(
        decide_capture(&text_clip("secret"), &s, Some(&src)),
        CaptureDecision::Skip("blacklisted")
    );
}

#[test]
fn blacklist_uses_platform_path_case_rules() {
    let mut s = settings();
    s.blacklist
        .push(r"C:\Program Files\Editor\EDITOR.EXE".into());
    let src = ClipSource {
        bundle_id: r"c:\program files\editor\editor.exe".into(),
        name: "Editor".into(),
    };
    if cfg!(target_os = "windows") {
        assert_eq!(
            decide_capture(&text_clip("secret"), &s, Some(&src)),
            CaptureDecision::Skip("blacklisted")
        );
    } else {
        assert!(matches!(
            decide_capture(&text_clip("secret"), &s, Some(&src)),
            CaptureDecision::Record { .. }
        ));
    }
}

#[test]
fn blacklist_survives_versioned_install_dir_upgrade() {
    // Discord 式安装目录带版本号，升级后全路径变化但文件名不变
    let mut s = settings();
    s.blacklist
        .push(r"C:\Users\a\AppData\Local\Discord\app-1.0.9016\Discord.exe".into());
    let upgraded = ClipSource {
        bundle_id: r"C:\Users\a\AppData\Local\Discord\app-1.0.9017\Discord.exe".into(),
        name: "Discord".into(),
    };
    if cfg!(target_os = "windows") {
        assert_eq!(
            decide_capture(&text_clip("secret"), &s, Some(&upgraded)),
            CaptureDecision::Skip("blacklisted")
        );
    } else {
        assert!(matches!(
            decide_capture(&text_clip("secret"), &s, Some(&upgraded)),
            CaptureDecision::Record { .. }
        ));
    }
}

#[test]
fn empty_and_oversize_text_skipped() {
    assert_eq!(
        decide_capture(&text_clip("   "), &settings(), None),
        CaptureDecision::Skip("empty text")
    );
    let mut s = settings();
    s.max_item_bytes = 4;
    assert_eq!(
        decide_capture(&text_clip("toolong"), &s, None),
        CaptureDecision::Skip("text too large")
    );
}

#[test]
fn text_classified_and_hashed() {
    let d = decide_capture(&text_clip("https://example.com/x"), &settings(), None);
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
        decide_capture(&c, &settings(), None),
        CaptureDecision::Record {
            kind: ClipKind::Files,
            ..
        }
    ));

    let mut limited = settings();
    limited.max_item_bytes = 3;
    assert_eq!(
        decide_capture(&c, &limited, None),
        CaptureDecision::Skip("files too large")
    );
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
        decide_capture(&clip, &limited, None),
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
        decide_capture(&big, &s, None),
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
        decide_capture(&small, &s2, None),
        CaptureDecision::Skip("image capture disabled")
    );
    assert!(matches!(
        decide_capture(&small, &settings(), None),
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
        decide_capture(&missing_dims, &settings(), None),
        CaptureDecision::Skip("invalid image")
    );

    let oversized_dims = CapturedClip {
        image_png: Some(vec![0u8; 10]),
        image_dims: Some((16_385, 1)),
        ..Default::default()
    };
    assert_eq!(
        decide_capture(&oversized_dims, &settings(), None),
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
        Some(vec!["a.img".into(), "a.thumb".into()])
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
