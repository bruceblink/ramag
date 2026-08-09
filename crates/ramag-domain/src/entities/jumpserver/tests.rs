use super::*;

#[test]
fn debug_output_redacts_password_and_token() {
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "secret-password".into(),
    };
    let session = JumpServerSession {
        base_url: credential.base_url.clone(),
        ssh_host: "jump.example.com".into(),
        ssh_port: credential.ssh_port,
        username: credential.username.clone(),
        password: credential.password.clone(),
        token_keyword: "Bearer".into(),
        token: "secret-token".into(),
        organizations: Vec::new(),
    };

    assert!(!format!("{credential:?}").contains("secret-password"));
    let connection = JumpServerConnection::new(credential.clone());
    assert!(!format!("{connection:?}").contains("secret-password"));
    let rendered = format!("{session:?}");
    assert!(!rendered.contains("secret-password"));
    assert!(!rendered.contains("secret-token"));
}

#[test]
fn credential_rejects_direct_login_delimiters() {
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice#root".into(),
        password: "password".into(),
    };

    assert!(credential.validate().is_err());
}

#[test]
fn direct_login_rejects_invalid_asset_and_account_identifiers() {
    let asset = JumpServerAsset {
        id: "../asset".into(),
        org_id: String::new(),
        name: "login".into(),
        address: "10.0.0.1".into(),
        platform: "Linux".into(),
        labels: Vec::new(),
        node_ids: Vec::new(),
        favorite: false,
        ungrouped: false,
        active: true,
    };
    let account = JumpServerAccount {
        id: "account-1".into(),
        alias: "account-1".into(),
        name: "root#ops".into(),
        username: "root".into(),
        has_secret: true,
        can_connect: true,
    };

    assert!(asset.validate_id().is_err());
    assert!(!account.usable_for_direct_login());
}

#[test]
fn direct_login_requires_connect_permission_not_managed_secret_flag() {
    let mut account = JumpServerAccount {
        id: "account-1".into(),
        alias: "account-1".into(),
        name: "root".into(),
        username: "root".into(),
        has_secret: false,
        can_connect: true,
    };

    assert!(account.usable_for_direct_login());
    account.can_connect = false;
    assert!(!account.usable_for_direct_login());
}

#[test]
fn web_session_requires_managed_secret_and_connect_permission() {
    let mut account = JumpServerAccount {
        id: "account-1".into(),
        alias: "account-1".into(),
        name: "Administrator".into(),
        username: "Administrator".into(),
        has_secret: true,
        can_connect: true,
    };

    assert!(account.validate_for_web_session().is_ok());
    account.has_secret = false;
    assert!(account.validate_for_web_session().is_err());
    account.has_secret = true;
    account.can_connect = false;
    assert!(account.validate_for_web_session().is_err());
}

fn rdp_session(index: u32) -> JumpServerRdpSession {
    JumpServerRdpSession {
        connection_id: "00000000-0000-0000-0000-000000000010".into(),
        jumpserver_url: "https://jump.example.com".into(),
        asset_id: format!("00000000-0000-0000-0000-{index:012}"),
        org_id: "org-1".into(),
        asset_name: format!("windows-{index}"),
        asset_address: format!("10.0.0.{index}"),
        asset_platform: "Windows".into(),
        account_id: "account-1".into(),
        account_name: "admin".into(),
        account_username: "Administrator".into(),
    }
}

#[test]
fn rdp_history_moves_sessions_between_recent_and_favorites_without_duplicates() {
    let first = rdp_session(1);
    let second = rdp_session(2);
    let mut history = JumpServerRdpSessionHistory::default();

    assert_eq!(history.record_open(first.clone()), Ok(()));
    assert_eq!(history.record_open(second.clone()), Ok(()));
    assert_eq!(history.recent, vec![second.clone(), first.clone()]);

    assert_eq!(history.set_favorite(&first, true), Ok(()));
    assert_eq!(history.favorites, vec![first.clone()]);
    assert_eq!(history.recent, vec![second.clone()]);

    let mut renamed = first.clone();
    renamed.asset_name = "windows-renamed".into();
    assert_eq!(history.record_open(renamed.clone()), Ok(()));
    assert_eq!(history.favorites, vec![renamed.clone()]);
    assert_eq!(history.recent, vec![second.clone()]);

    assert_eq!(history.set_favorite(&renamed, false), Ok(()));
    assert!(history.favorites.is_empty());
    assert_eq!(history.recent, vec![renamed, second]);
    assert_eq!(history.validate(), Ok(()));
}

#[test]
fn rdp_history_bounds_recent_sessions_and_rejects_invalid_targets() {
    let mut history = JumpServerRdpSessionHistory::default();
    for index in 1..=(MAX_JUMPSERVER_RDP_RECENT_SESSIONS as u32 + 1) {
        assert_eq!(history.record_open(rdp_session(index)), Ok(()));
    }
    assert_eq!(history.recent.len(), MAX_JUMPSERVER_RDP_RECENT_SESSIONS);
    assert_eq!(history.recent[0].asset_name, "windows-21");

    let mut invalid = rdp_session(1);
    invalid.connection_id = "not-a-uuid".into();
    assert!(invalid.validate().is_err());
    assert!(history.record_open(invalid).is_err());
}

#[test]
fn rdp_favorites_are_sorted_by_asset_name() {
    let mut alpha = rdp_session(1);
    alpha.asset_name = "Alpha".into();
    let mut beta = rdp_session(2);
    beta.asset_name = "beta".into();
    let mut zeta = rdp_session(3);
    zeta.asset_name = "zeta".into();
    let mut history = JumpServerRdpSessionHistory::default();

    assert_eq!(history.set_favorite(&zeta, true), Ok(()));
    assert_eq!(history.set_favorite(&beta, true), Ok(()));
    assert_eq!(history.set_favorite(&alpha, true), Ok(()));

    assert_eq!(history.favorites, vec![alpha, beta, zeta]);
}
