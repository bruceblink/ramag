use super::*;
use ramag_domain::entities::{RemotePath, SshProfileOrigin};

#[test]
fn windows_compatibility_bootstrap_lists_the_remote_drive_root_first() {
    let mut profile = SshProfile::new("asset", "jump.example.com");
    profile.origin = SshProfileOrigin::JumpServer;
    profile.remote_platform = RemotePlatformPreference::Windows;
    profile.windows_sftp_compatibility = true;
    profile.username = "axemc_li#Administrator#asset-1".into();

    let candidates = bootstrap_directory_candidates(&profile, ".");

    assert_eq!(candidates, ["/"]);

    assert_eq!(bootstrap_directory_candidates(&profile, "/"), ["/"]);
}

#[test]
fn detected_windows_platform_is_applied_to_auto_file_operations() {
    let mut profile = SshProfile::new("asset", "jump.example.com");
    profile.remote_platform = RemotePlatformPreference::Auto;
    let capabilities = SshRemoteCapabilities {
        operating_system: RemoteOperatingSystem::Windows,
        ..SshRemoteCapabilities::default()
    };

    let effective = profile_for_capabilities(&profile, &capabilities);

    assert_eq!(effective.remote_platform, RemotePlatformPreference::Windows);
    assert_eq!(profile.remote_platform, RemotePlatformPreference::Auto);

    profile.remote_platform = RemotePlatformPreference::Linux;
    assert_eq!(
        profile_for_capabilities(&profile, &capabilities).remote_platform,
        RemotePlatformPreference::Linux
    );
}

#[test]
fn windows_sftp_write_is_allowed_when_capability_is_available() {
    let profile = SshProfile::new("windows", "windows.example.com");
    let capabilities = SshRemoteCapabilities {
        operating_system: RemoteOperatingSystem::Windows,
        sftp: RemoteCapabilityState::Available,
        ..SshRemoteCapabilities::default()
    };

    assert!(ensure_remote_write_platform(&profile, &capabilities).is_ok());
}

#[test]
fn cached_non_root_sftp_path_survives_late_root_probe() {
    let cached = SshRemoteCapabilities {
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: SftpNamespaceKind::WindowsDrive,
        sftp_canonical_path: Some(
            RemotePath::parse_server_canonical("C:/Users/Administrator").unwrap(),
        ),
        ..SshRemoteCapabilities::default()
    };
    let fresh = SshRemoteCapabilities {
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: SftpNamespaceKind::Posix,
        sftp_canonical_path: Some(RemotePath::parse_server_canonical("/").unwrap()),
        ..SshRemoteCapabilities::default()
    };

    assert!(should_keep_bootstrapped_sftp(&cached, &fresh));
}

#[test]
fn discovered_windows_drive_root_survives_a_late_posix_root_probe() {
    let cached = SshRemoteCapabilities {
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: SftpNamespaceKind::Virtual,
        sftp_canonical_path: Some(
            RemotePath::parse_with_namespace("/", SftpNamespaceKind::Virtual).unwrap(),
        ),
        ..SshRemoteCapabilities::default()
    };
    let fresh = SshRemoteCapabilities {
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: SftpNamespaceKind::Posix,
        sftp_canonical_path: Some(RemotePath::parse_server_canonical("/").unwrap()),
        ..SshRemoteCapabilities::default()
    };

    assert!(should_keep_bootstrapped_sftp(&cached, &fresh));
    assert!(is_virtual_windows_path("/C:/"));
    assert!(!is_virtual_windows_path("C:/"));
}

#[test]
fn discovered_windows_drive_root_is_preferred_over_a_single_home_hint() {
    let cached = SshRemoteCapabilities {
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: SftpNamespaceKind::Virtual,
        sftp_canonical_path: Some(
            RemotePath::parse_with_namespace("/", SftpNamespaceKind::Virtual).unwrap(),
        ),
        ..SshRemoteCapabilities::default()
    };
    let fresh = SshRemoteCapabilities {
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: SftpNamespaceKind::WindowsDrive,
        sftp_canonical_path: Some(
            RemotePath::parse_server_canonical("C:/Users/Administrator").unwrap(),
        ),
        ..SshRemoteCapabilities::default()
    };

    assert!(should_keep_bootstrapped_sftp(&cached, &fresh));
}
