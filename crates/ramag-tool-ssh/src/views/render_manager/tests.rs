use ramag_domain::entities::{RemotePlatformPreference, SshAuthMode, SshProfile, SshProfileOrigin};

use super::{
    environment_badge_colors, is_jumpserver_profile, platform_label, profile_matches_query,
};

#[test]
fn environment_presets_have_distinct_badge_colors() {
    let fallback = gpui::black();
    assert_ne!(
        environment_badge_colors("dev", fallback).0,
        environment_badge_colors("prod", fallback).0
    );
}

#[test]
fn profile_search_matches_name_host_and_username() {
    let mut profile = SshProfile::new("Production", "SERVER.EXAMPLE");
    profile.username = "Alice".into();
    profile.environment = Some("staging".into());

    assert!(profile_matches_query(&profile, "production"));
    assert!(profile_matches_query(&profile, "server.example"));
    assert!(profile_matches_query(&profile, "alice"));
    assert!(profile_matches_query(&profile, "staging"));
    assert!(!profile_matches_query(&profile, "missing"));
}

#[test]
fn jumpserver_icon_supports_explicit_and_legacy_profiles() {
    let mut explicit = SshProfile::new("asset", "jump.example");
    explicit.origin = SshProfileOrigin::JumpServer;
    assert!(is_jumpserver_profile(&explicit));

    let mut legacy = SshProfile::new("asset", "jump.example");
    legacy.auth_mode = SshAuthMode::Password;
    legacy.username = "login#root#00000000-0000-0000-0000-000000000000".into();
    assert!(is_jumpserver_profile(&legacy));

    legacy.username = "ordinary-user".into();
    assert!(!is_jumpserver_profile(&legacy));
}

#[test]
fn platform_labels_are_stable_for_manager_badges() {
    assert_eq!(platform_label(RemotePlatformPreference::Windows), "Windows");
    assert_eq!(platform_label(RemotePlatformPreference::Linux), "Linux");
    assert_eq!(platform_label(RemotePlatformPreference::Auto), "自动");
}
