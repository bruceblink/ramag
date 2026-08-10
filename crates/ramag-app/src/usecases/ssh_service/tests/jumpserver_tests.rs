use super::*;

#[test]
fn jumpserver_test_and_save_refresh_asset_detail() {
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let jumpserver: Arc<dyn JumpServerDriver> = Arc::new(CountingJumpServerDriver {
        detail_calls: detail_calls.clone(),
        web_session_calls: Arc::new(AtomicUsize::new(0)),
    });
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()))
        .with_jumpserver_driver(jumpserver);
    let session = jumpserver_session();
    let asset = jumpserver_asset();

    let tested =
        futures::executor::block_on(service.test_jumpserver_asset(&session, &asset, "account-1"))
            .unwrap();
    let saved = futures::executor::block_on(service.save_jumpserver_asset_for_connection(
        "00000000-0000-0000-0000-000000000002",
        &session,
        &asset,
        "account-1",
    ))
    .unwrap();

    assert_eq!(detail_calls.load(Ordering::SeqCst), 2);
    for profile in [&tested, &saved] {
        assert_eq!(profile.host, "jump.example.com");
        assert_eq!(profile.port, Some(2222));
        assert_eq!(
            profile.username,
            "alice#root#00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(profile.auth_mode, SshAuthMode::Password);
        assert_eq!(profile.origin, SshProfileOrigin::JumpServer);
        assert_eq!(profile.password, "login-password");
    }
    assert!(tested.jumpserver_rdp_session.is_none());
    let rdp_session = saved
        .jumpserver_rdp_session
        .as_ref()
        .expect("导入时应记录可复用的远程桌面目标");
    assert_eq!(
        rdp_session.connection_id,
        "00000000-0000-0000-0000-000000000002"
    );
    assert_eq!(rdp_session.account_id, "account-1");
}

#[test]
fn jumpserver_rdp_web_session_refreshes_detail_and_uses_selected_account() {
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let web_session_calls = Arc::new(AtomicUsize::new(0));
    let jumpserver: Arc<dyn JumpServerDriver> = Arc::new(CountingJumpServerDriver {
        detail_calls: detail_calls.clone(),
        web_session_calls: web_session_calls.clone(),
    });
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()))
        .with_jumpserver_driver(jumpserver);

    let url = futures::executor::block_on(service.create_jumpserver_rdp_web_session(
        &jumpserver_session(),
        &jumpserver_asset(),
        "account-1",
    ))
    .unwrap();

    assert_eq!(
        url,
        "https://jump.example.com/lion/connect?token=session-token"
    );
    assert_eq!(detail_calls.load(Ordering::SeqCst), 1);
    assert_eq!(web_session_calls.load(Ordering::SeqCst), 1);
}

fn jumpserver_rdp_record(connection_id: String) -> JumpServerRdpSession {
    JumpServerRdpSession {
        connection_id,
        jumpserver_url: "https://jump.example.com".into(),
        asset_id: "00000000-0000-0000-0000-000000000001".into(),
        org_id: "org-1".into(),
        asset_name: "windows-prod".into(),
        asset_address: "10.0.0.2".into(),
        asset_platform: "Windows".into(),
        account_id: "account-1".into(),
        account_name: "admin".into(),
        account_username: "Administrator".into(),
    }
}

#[test]
fn jumpserver_rdp_history_is_encrypted_and_supports_favorites() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let record = jumpserver_rdp_record("00000000-0000-0000-0000-000000000010".into());

    let recent =
        futures::executor::block_on(service.record_jumpserver_rdp_session(record.clone())).unwrap();
    assert_eq!(recent.recent, vec![record.clone()]);
    let stored = storage
        .preferences
        .lock()
        .get("ssh_jumpserver_rdp_sessions_v1")
        .cloned()
        .unwrap();
    assert!(stored.starts_with("enc-v1:"));
    assert!(!stored.contains("windows-prod"));

    let favorite =
        futures::executor::block_on(service.set_jumpserver_rdp_session_favorite(&record, true))
            .unwrap();
    assert_eq!(favorite.favorites, vec![record.clone()]);
    assert!(favorite.recent.is_empty());

    let recent_again =
        futures::executor::block_on(service.set_jumpserver_rdp_session_favorite(&record, false))
            .unwrap();
    assert!(recent_again.favorites.is_empty());
    assert_eq!(recent_again.recent, vec![record]);
}

#[test]
fn saved_jumpserver_rdp_session_reauthenticates_and_revalidates_target() {
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let web_session_calls = Arc::new(AtomicUsize::new(0));
    let storage = Arc::new(NoopStorage::default());
    let jumpserver: Arc<dyn JumpServerDriver> = Arc::new(CountingJumpServerDriver {
        detail_calls: detail_calls.clone(),
        web_session_calls: web_session_calls.clone(),
    });
    let service =
        SshService::new(Arc::new(TerminalDriver), storage).with_jumpserver_driver(jumpserver);
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "secret-password".into(),
    };
    let connection =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();
    let record = jumpserver_rdp_record(connection.id);

    let url = futures::executor::block_on(service.create_saved_jumpserver_rdp_web_session(&record))
        .unwrap();

    assert_eq!(
        url,
        "https://jump.example.com/lion/connect?token=session-token"
    );
    assert_eq!(detail_calls.load(Ordering::SeqCst), 1);
    assert_eq!(web_session_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn jumpserver_profile_uses_connect_permission_not_managed_secret_flag() {
    let mut detail = JumpServerAssetDetail {
        asset: jumpserver_asset(),
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "root".into(),
            username: "root".into(),
            has_secret: false,
            can_connect: true,
        }],
        ssh_enabled: true,
        rdp_web_enabled: false,
    };

    assert!(
        super::jumpserver::build_jumpserver_profile(&jumpserver_session(), &detail, "account-1")
            .is_ok()
    );
    detail.accounts[0].can_connect = false;
    assert!(
        super::jumpserver::build_jumpserver_profile(&jumpserver_session(), &detail, "account-1")
            .is_err()
    );
}

#[test]
fn jumpserver_windows_asset_preserves_remote_platform_preference() {
    let mut detail = JumpServerAssetDetail {
        asset: jumpserver_asset(),
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "administrator".into(),
            username: "Administrator".into(),
            has_secret: true,
            can_connect: true,
        }],
        ssh_enabled: true,
        rdp_web_enabled: true,
    };
    detail.asset.platform = "Windows".into();

    let profile =
        super::jumpserver::build_jumpserver_profile(&jumpserver_session(), &detail, "account-1")
            .unwrap();

    assert_eq!(profile.remote_platform, RemotePlatformPreference::Windows);
    assert!(!profile.windows_sftp_compatibility);
    assert_eq!(profile.rdp_web_enabled, Some(true));
}

#[test]
fn jumpserver_profile_omits_unavailable_remote_desktop_target() {
    let mut detail = JumpServerAssetDetail {
        asset: jumpserver_asset(),
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "administrator".into(),
            username: "Administrator".into(),
            has_secret: true,
            can_connect: true,
        }],
        ssh_enabled: true,
        rdp_web_enabled: false,
    };
    let session = jumpserver_session();
    let mut profile =
        super::jumpserver::build_jumpserver_profile(&session, &detail, "account-1").unwrap();

    super::jumpserver::attach_jumpserver_rdp_session(
        &mut profile,
        "00000000-0000-0000-0000-000000000002",
        &session,
        &detail,
        "account-1",
    )
    .unwrap();
    assert!(profile.jumpserver_rdp_session.is_none());

    detail.rdp_web_enabled = true;
    detail.accounts[0].has_secret = false;
    super::jumpserver::attach_jumpserver_rdp_session(
        &mut profile,
        "00000000-0000-0000-0000-000000000002",
        &session,
        &detail,
        "account-1",
    )
    .unwrap();
    assert!(profile.jumpserver_rdp_session.is_none());
}

#[test]
fn jumpserver_connections_are_encrypted_updated_and_deleted() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "secret-password".into(),
    };

    let first =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();
    let stored = storage
        .preferences
        .lock()
        .get("ssh_jumpserver_connections_v2")
        .cloned()
        .unwrap();
    assert!(stored.starts_with("enc-v1:"));
    assert!(!stored.contains("secret-password"));
    let loaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(loaded, vec![first.clone()]);

    let mut updated_credential = credential;
    updated_credential.ssh_port = 2200;
    let updated = futures::executor::block_on(
        service.save_jumpserver_connection(Some(&first.id), &updated_credential),
    )
    .unwrap();
    assert_eq!(updated.id, first.id);
    assert_eq!(updated.credential.ssh_port, 2200);

    futures::executor::block_on(service.delete_jumpserver_connection(&first.id)).unwrap();
    assert!(
        futures::executor::block_on(service.load_jumpserver_connections())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn jumpserver_connections_deduplicate_same_login_and_keep_latest_password() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let mut credential = JumpServerCredential {
        base_url: "https://jump.example.com/".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "old-password".into(),
    };

    let first =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();
    credential.base_url = "HTTPS://JUMP.EXAMPLE.COM".into();
    credential.password = "new-password".into();
    let updated =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();

    let loaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(updated.id, first.id);
    assert_eq!(loaded[0].credential.password, "new-password");
}

#[test]
fn jumpserver_connections_remove_existing_duplicate_records_when_loading() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "new-password".into(),
    };
    let newest = JumpServerConnection::new(credential.clone());
    let mut older_credential = credential;
    older_credential.password = "old-password".into();
    let older = JumpServerConnection::new(older_credential);
    let encoded = format!(
        "enc-v1:{}",
        hex::encode(serde_json::to_vec(&vec![newest.clone(), older]).unwrap())
    );
    storage
        .preferences
        .lock()
        .insert("ssh_jumpserver_connections_v2".into(), encoded);

    let loaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(loaded, vec![newest]);
    let reloaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(reloaded.len(), 1);
}

#[test]
fn jumpserver_legacy_credential_is_migrated_to_connection_list() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let credential = JumpServerCredential {
        base_url: "https://legacy.example.com".into(),
        ssh_port: 2222,
        username: "legacy".into(),
        password: "secret-password".into(),
    };
    let encoded = format!(
        "enc-v1:{}",
        hex::encode(serde_json::to_vec(&credential).unwrap())
    );
    storage
        .preferences
        .lock()
        .insert("ssh_jumpserver_credential_v1".into(), encoded);

    let migrated = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0].credential, credential);
    assert!(JumpServerConnection::validate(&migrated[0]).is_ok());
    let preferences = storage.preferences.lock();
    assert!(!preferences.contains_key("ssh_jumpserver_credential_v1"));
    assert!(preferences.contains_key("ssh_jumpserver_connections_v2"));
}
