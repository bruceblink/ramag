use super::*;

#[test]
fn default_driver_can_be_created_without_runtime_side_effects() {
    let _driver = OpenSshDriver::default();
}

#[test]
fn production_profile_is_rejected_before_infra_write_connection() {
    let mut profile = SshProfile::new("production", "server.example");
    profile.production = true;

    assert!(matches!(
        validate_writable_profile_and_path(&profile, "/remote/file"),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
    assert!(validate_profile_and_path(&profile, "/remote/file").is_ok());
}

#[test]
fn windows_sftp_home_tries_native_and_virtual_drive_forms() {
    assert_eq!(
        windows_sftp_home_candidates("C:/Users/Administrator"),
        ["C:/Users/Administrator", "/C:/Users/Administrator"]
    );
    assert!(windows_sftp_home_candidates("relative/path").is_empty());
}

#[test]
fn jumpserver_windows_default_does_not_escape_the_authorized_root() {
    let mut profile = SshProfile::new("windows", "jump.example.com");
    profile.origin = ramag_domain::entities::SshProfileOrigin::JumpServer;
    profile.username = "alice#administrator#asset-id".into();

    let candidates = windows_sftp_default_candidates(&profile, None);

    assert!(candidates.is_empty());
}

#[test]
fn windows_drive_discovery_runs_for_every_accessible_windows_transport_root() {
    let mut windows = SshProfile::new("windows", "windows.example");
    windows.remote_platform = RemotePlatformPreference::Windows;

    assert!(should_list_windows_drives(&windows, "."));
    assert!(should_list_windows_drives(&windows, "/"));
    assert!(!should_list_windows_drives(
        &windows,
        "C:/Users/Administrator"
    ));

    let linux = SshProfile::new("linux", "linux.example");
    assert!(!should_list_windows_drives(&linux, "/"));

    let mut jumpserver = SshProfile::new("jumpserver", "jump.example");
    jumpserver.origin = SshProfileOrigin::JumpServer;
    assert!(!should_list_windows_drives(&jumpserver, "/"));
    jumpserver.remote_platform = RemotePlatformPreference::Windows;
    assert!(!should_list_windows_drives(&jumpserver, "/"));
    jumpserver.windows_sftp_compatibility = true;
    assert!(should_list_windows_drives(&jumpserver, "/"));
    jumpserver.production = true;
    assert!(should_list_windows_drives(&jumpserver, "/"));
}

#[test]
fn auto_profile_uses_the_detected_platform_for_sftp_only() {
    let mut profile = SshProfile::new("jumpserver", "jump.example.com");
    profile.remote_platform = RemotePlatformPreference::Auto;
    profile.origin = SshProfileOrigin::JumpServer;

    let effective = profile_with_detected_platform(&profile, RemoteOperatingSystem::Windows);

    assert_eq!(effective.remote_platform, RemotePlatformPreference::Windows);
    assert_eq!(effective.id, profile.id);
    assert_eq!(effective.username, profile.username);
    assert_eq!(profile.remote_platform, RemotePlatformPreference::Auto);
}

#[test]
fn windows_compatibility_transport_confirms_unknown_platform() {
    let mut capabilities = SshRemoteCapabilities {
        sftp: RemoteCapabilityState::Available,
        operating_system: RemoteOperatingSystem::Unknown,
        ..SshRemoteCapabilities::default()
    };

    apply_sftp_transport_evidence(&mut capabilities, SftpTransportKind::WindowsCompatibility);

    assert_eq!(
        capabilities.operating_system,
        RemoteOperatingSystem::Windows
    );
}

#[test]
fn standard_sftp_transport_does_not_guess_platform() {
    let mut capabilities = SshRemoteCapabilities::default();

    apply_sftp_transport_evidence(&mut capabilities, SftpTransportKind::StandardSubsystem);

    assert_eq!(
        capabilities.operating_system,
        RemoteOperatingSystem::Unknown
    );
}
