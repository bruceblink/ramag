//! OpenSSH 驱动的连接、路径和诊断辅助逻辑。

use super::*;

pub(super) async fn connect(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
) -> Result<std::sync::Arc<session::SftpConnection>> {
    let capability = locator.probe(profile.ssh_path.clone()).await?;
    let (args, transport) = if uses_windows_remote_sftp(profile) {
        (
            windows_remote_sftp_args(profile)?,
            session::SftpTransport::WindowsRemoteServer,
        )
    } else {
        (sftp_args(profile)?, session::SftpTransport::Subsystem)
    };
    match sessions
        .get_or_connect(profile, &capability.executable, &args, transport)
        .await
    {
        Ok(connection) => Ok(connection),
        Err(error) => {
            tracing::warn!(
                profile_id = %profile.id,
                transport = ?transport,
                error = %error,
                "ssh sftp session connection failed"
            );
            locator.invalidate(&capability.executable);
            Err(error)
        }
    }
}

pub(super) async fn canonicalize_sftp_initial_directory(
    session: &session::StructuredSftpSession,
    profile: &SshProfile,
    operating_system: RemoteOperatingSystem,
    default_directory_hint: Option<&str>,
) -> std::result::Result<String, russh_sftp::client::error::Error> {
    let canonical = session
        .canonicalize(profile.initial_path().to_string())
        .await?;
    let windows = operating_system == RemoteOperatingSystem::Windows
        || (operating_system == RemoteOperatingSystem::Unknown
            && profile.remote_platform == RemotePlatformPreference::Windows);
    if profile.initial_directory.is_some() || !windows || canonical != "/" {
        return Ok(canonical);
    }
    // JumpServer 的 SFTP 路径位于服务端授权根目录下，不能用终端里的 Windows
    // 绝对路径继续探测，否则 `C:/` 会被错误解释为授权根下的相对目录。
    if profile.origin == SshProfileOrigin::JumpServer {
        return Ok(canonical);
    }
    for candidate in windows_sftp_default_candidates(profile, default_directory_hint) {
        if let Ok(resolved) = session.canonicalize(candidate).await
            && resolved != "/"
        {
            return Ok(resolved);
        }
    }
    Ok(canonical)
}

pub(super) fn windows_sftp_default_candidates(
    profile: &SshProfile,
    default_directory_hint: Option<&str>,
) -> Vec<String> {
    if profile.origin == SshProfileOrigin::JumpServer {
        return Vec::new();
    }
    let mut candidates = default_directory_hint
        .into_iter()
        .flat_map(windows_sftp_home_candidates)
        .collect::<Vec<_>>();
    let account = Some(profile.username.as_str());
    if let Some(account) = account.filter(|account| !account.is_empty()) {
        candidates.extend(windows_sftp_home_candidates(&format!("C:/Users/{account}")));
    }
    candidates.extend([
        "C:/Users".into(),
        "/C:/Users".into(),
        "C:/".into(),
        "/C:/".into(),
    ]);
    candidates.dedup();
    candidates
}

pub(super) fn windows_sftp_home_candidates(home: &str) -> Vec<String> {
    if RemotePath::parse_server_canonical(home).is_err() {
        return Vec::new();
    }
    let mut candidates = vec![home.to_string()];
    if infer_sftp_namespace(home) == SftpNamespaceKind::WindowsDrive {
        candidates.push(format!("/{home}"));
    }
    candidates
}

pub(super) async fn list_once(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
    path: &str,
) -> Result<RemoteDirectory> {
    let connection = connect(locator, sessions, profile).await?;
    let max_entries = if profile.production {
        MAX_PRODUCTION_DIRECTORY_ENTRIES
    } else {
        MAX_REMOTE_DIRECTORY_ENTRIES
    };
    let result = timeout(DIRECTORY_REQUEST_TIMEOUT, async {
        let mut directory = connection
            .contextualize(session::list_directory(&connection.session, path, max_entries).await)?;
        if should_list_windows_drives(profile, path) {
            match timeout(
                WINDOWS_DRIVE_DISCOVERY_TIMEOUT,
                session::list_windows_drives(&connection.session),
            )
            .await
            {
                Ok(drives) if !drives.is_empty() => {
                    directory.path = "/".into();
                    directory.entries = drives;
                }
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        profile_id = %profile.id,
                        "windows sftp drive discovery timed out"
                    );
                }
            }
        }
        Ok(directory)
    })
    .await;
    match result {
        Ok(directory) => directory,
        Err(_) => {
            tracing::warn!(
                profile_id = %profile.id,
                path,
                timeout_seconds = DIRECTORY_REQUEST_TIMEOUT.as_secs(),
                "ssh sftp directory request timed out"
            );
            sessions.invalidate(&profile.id).await;
            Err(DomainError::ConnectionFailed(format!(
                "读取远程目录超时（{} 秒）",
                DIRECTORY_REQUEST_TIMEOUT.as_secs()
            )))
        }
    }
}

pub(super) fn should_list_windows_drives(profile: &SshProfile, requested_path: &str) -> bool {
    profile.remote_platform == RemotePlatformPreference::Windows
        && (profile.origin != SshProfileOrigin::JumpServer || uses_windows_remote_sftp(profile))
        && matches!(requested_path, "." | "/")
}

pub(super) fn profile_with_detected_platform(
    profile: &SshProfile,
    operating_system: RemoteOperatingSystem,
) -> SshProfile {
    let mut effective = profile.clone();
    if effective.remote_platform == RemotePlatformPreference::Auto {
        effective.remote_platform = match operating_system {
            RemoteOperatingSystem::Linux => RemotePlatformPreference::Linux,
            RemoteOperatingSystem::Windows => RemotePlatformPreference::Windows,
            RemoteOperatingSystem::Unknown => RemotePlatformPreference::Auto,
        };
    }
    effective
}

pub(super) async fn execute_file_metadata(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    path: &RemotePath,
) -> Result<SshDiagnosticResult> {
    validate_diagnostic_path(capabilities, path)?;
    let started = Instant::now();
    let connection = connect(locator, sessions, profile).await?;
    let metadata = connection
        .session
        .raw
        .lstat(path.canonical().to_string())
        .await
        .map_err(|error| session::map_sftp_error("读取远程文件元信息", error))?;
    let attrs = metadata.attrs;
    let kind = if attrs.is_regular() {
        "file"
    } else if attrs.is_dir() {
        "directory"
    } else if attrs.is_symlink() {
        "symlink"
    } else {
        "other"
    };
    let output = serde_json::to_string(&serde_json::json!({
        "path": path.canonical(),
        "kind": kind,
        "size": attrs.size,
        "permissions": attrs.permissions,
        "modifiedAtUnix": attrs.mtime,
    }))
    .map_err(|error| DomainError::Other(format!("序列化远程文件元信息失败：{error}")))?;
    diagnostic_result(profile, capabilities, "file_metadata", output, started)
}

pub(super) async fn execute_file_chunk(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    path: &RemotePath,
    position: RemoteFileChunkPosition,
) -> Result<SshDiagnosticResult> {
    validate_diagnostic_path(capabilities, path)?;
    let started = Instant::now();
    let connection = connect(locator, sessions, profile).await?;
    let chunk = session::read_file_chunk(&connection.session, path.canonical(), position).await?;
    let text = String::from_utf8(chunk.bytes)
        .map_err(|_| DomainError::InvalidConfig("安全诊断文件片段只支持有效 UTF-8 文本".into()))?;
    let output = sanitize_diagnostic_text(&text);
    diagnostic_result(profile, capabilities, "file_chunk", output, started)
}

pub(super) fn validate_diagnostic_path(
    capabilities: &SshRemoteCapabilities,
    path: &RemotePath,
) -> Result<()> {
    if capabilities.sftp != RemoteCapabilityState::Available {
        return Err(DomainError::Forbidden("当前连接的 SFTP 能力不可用".into()));
    }
    if path.namespace() != capabilities.sftp_namespace {
        return Err(DomainError::InvalidConfig(
            "诊断路径不属于当前 SFTP 命名空间".into(),
        ));
    }
    Ok(())
}

pub(super) fn diagnostic_result(
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    operation: &str,
    output: String,
    started: Instant,
) -> Result<SshDiagnosticResult> {
    let provider = capabilities
        .diagnostic_provider
        .ok_or_else(|| DomainError::Forbidden("当前连接没有可用的安全诊断提供者".into()))?;
    Ok(SshDiagnosticResult {
        profile_id: profile.id.clone(),
        operation: operation.into(),
        operating_system: capabilities.operating_system,
        provider,
        output,
        exit_code: Some(0),
        termination: DiagnosticTermination::Completed,
        truncated: false,
        elapsed_millis: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

pub(super) fn sanitize_diagnostic_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\r' | '\t') || !character.is_control())
        .collect()
}

pub(super) async fn invalidate_broken<T>(
    sessions: &SessionCache,
    profile_id: &SshProfileId,
    result: &Result<T>,
) {
    if matches!(result, Err(DomainError::ConnectionFailed(_))) {
        sessions.invalidate(profile_id).await;
    }
}

pub(super) fn validate_profile_and_path(profile: &SshProfile, path: &str) -> Result<()> {
    profile.validate().map_err(DomainError::InvalidConfig)?;
    validate_remote_path(path).map_err(DomainError::InvalidConfig)
}

pub(super) fn validate_writable_profile_and_path(profile: &SshProfile, path: &str) -> Result<()> {
    validate_profile_and_path(profile, path)?;
    if profile.production {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    Ok(())
}
