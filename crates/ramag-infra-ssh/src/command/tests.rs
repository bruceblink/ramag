//! 拆分后的测试模块。

use super::*;

fn profile() -> SshProfile {
    let mut profile = SshProfile::new("server", "example.com");
    profile.port = Some(2222);
    profile.username = "alice".into();
    profile
}

#[test]
fn command_arguments_are_separate_and_end_options_before_host() {
    let mut profile = profile();
    profile.host = "server; touch /tmp/bad".into();
    assert!(
        terminal_command(
            &profile,
            &SshCapability {
                executable: "/usr/bin/ssh".into(),
                version: "OpenSSH_test".into(),
            },
            None,
        )
        .is_err()
    );

    profile.host = "server.example".into();
    let terminal = terminal_command(
        &profile,
        &SshCapability {
            executable: "/usr/bin/ssh".into(),
            version: "OpenSSH_test".into(),
        },
        None,
    )
    .unwrap();
    assert_eq!(
        &terminal.args[terminal.args.len() - 2..],
        ["--", "server.example"]
    );

    let sftp = sftp_args(&profile).unwrap();
    assert_eq!(&sftp[sftp.len() - 3..], ["--", "server.example", "sftp"]);
    assert!(sftp.windows(2).any(|args| args == ["-o", "BatchMode=yes"]));
    assert!(!sftp.iter().any(|arg| arg == "StrictHostKeyChecking=no"));
}

#[test]
fn production_sftp_disables_forwarding_and_remote_command() {
    let mut production = profile();
    production.production = true;
    let args = sftp_args(&production).unwrap();
    for option in [
        "ClearAllForwardings=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "RequestTTY=no",
        "ControlMaster=no",
        "ControlPath=none",
        "Tunnel=no",
    ] {
        assert!(args.iter().any(|arg| arg == option), "missing {option}");
    }

    let normal = sftp_args(&profile()).unwrap();
    assert!(!normal.iter().any(|arg| arg == "RemoteCommand=none"));
}

#[test]
fn windows_compatibility_mode_uses_a_fixed_remote_sftp_server_command() {
    let mut profile = profile();
    profile.remote_platform = RemotePlatformPreference::Windows;
    assert!(!uses_windows_remote_sftp(&profile));
    assert!(windows_remote_sftp_args(&profile).is_err());

    profile.windows_sftp_compatibility = true;
    let args = windows_remote_sftp_args(&profile).unwrap();
    let command = args.last().unwrap();

    assert!(uses_windows_remote_sftp(&profile));
    assert_eq!(command, WINDOWS_SFTP_SERVER_COMMAND);
    assert!(!command.contains(&profile.username));
    assert!(!command.contains("Invoke-Expression"));

    profile.production = true;
    assert!(uses_windows_remote_sftp(&profile));
    assert!(windows_remote_sftp_args(&profile).is_ok());
}

#[test]
fn terminal_probe_is_fixed_and_available_in_production() {
    let profile = profile();
    let args = terminal_probe_args(&profile).unwrap();
    assert_eq!(args.last().map(String::as_str), Some("exit"));
    assert!(args.iter().any(|argument| argument == "-tt"));

    let mut production = profile;
    production.production = true;
    assert!(terminal_probe_args(&production).is_ok());
}

#[test]
fn terminal_directory_keeps_the_interactive_login_command() {
    let profile = profile();
    let capability = SshCapability {
        executable: "/usr/bin/ssh".into(),
        version: "OpenSSH_test".into(),
    };

    let terminal =
        terminal_command(&profile, &capability, Some("/srv/team's data/$(whoami)")).unwrap();

    assert_eq!(
        &terminal.args[terminal.args.len() - 2..],
        ["--", "example.com"]
    );
    assert!(terminal_command(&profile, &capability, Some("relative/path")).is_err());
}

#[test]
fn config_alias_keeps_config_port_and_hash_username_is_literal() {
    let mut profile = SshProfile::new("jump", "private-jump");
    profile.port = None;
    let args = sftp_args(&profile).unwrap();
    assert!(!args.iter().any(|arg| arg == "-p"));
    assert!(!args.iter().any(|arg| arg == "-l"));

    profile.username = "team#account#00000000-0000-0000-0000-000000000000".into();
    let args = sftp_args(&profile).unwrap();
    assert!(
        args.windows(2)
            .any(|args| { args == ["-l", "team#account#00000000-0000-0000-0000-000000000000"] })
    );
}

#[test]
fn password_mode_enables_one_askpass_attempt_without_exposing_secret() {
    let mut profile = profile();
    profile.auth_mode = SshAuthMode::Password;
    profile.password = "top-secret".into();

    let args = sftp_args(&profile).unwrap();
    assert!(args.windows(2).any(|args| args == ["-o", "BatchMode=no"]));
    assert!(
        args.windows(2)
            .any(|args| args == ["-o", "NumberOfPasswordPrompts=1"])
    );
    assert!(args.iter().all(|arg| !arg.contains("top-secret")));
}

#[test]
fn custom_executable_must_be_absolute() {
    let error = discover_candidates(Some("ssh")).unwrap_err();
    assert!(matches!(error, DomainError::InvalidConfig(_)));
}

#[tokio::test]
async fn output_is_bounded_and_sanitized() {
    let input = vec![b'a'; PROBE_OUTPUT_LIMIT + 100];
    assert_eq!(
        read_bounded(input.as_slice()).await.unwrap().len(),
        PROBE_OUTPUT_LIMIT
    );
    assert_eq!(
        sanitized_output(b"OpenSSH_test\0secret\nsecond"),
        "OpenSSH_testsecret"
    );
}

#[test]
fn automatic_discovery_never_uses_current_directory() {
    let current = dunce::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let candidates = discover_candidates(None).unwrap();
    assert!(candidates.iter().all(|candidate| {
        candidate
            .parent()
            .is_none_or(|directory| !paths_equal(directory, &current))
    }));
}
