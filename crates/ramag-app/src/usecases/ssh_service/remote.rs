//! SSH 终端门禁、远端能力、诊断与 SFTP 路径编排。

use super::*;

impl SshService {
    pub async fn probe(&self, custom_path: Option<&str>) -> Result<SshCapability> {
        self.driver.probe(custom_path).await
    }

    pub async fn test_connection(&self, profile: &SshProfile) -> Result<SshRemoteCapabilities> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        self.driver.probe_remote_capabilities(profile).await
    }

    pub async fn probe_remote_capabilities(
        &self,
        profile_id: &SshProfileId,
    ) -> Result<SshRemoteCapabilities> {
        let profile = self.current_profile(profile_id).await?;
        self.capabilities_for_profile(&profile, true).await
    }

    /// 无 PTY 探测被堡垒机拦截时，交互终端提示符仍可提供可靠的平台证据。
    pub fn remember_terminal_windows(
        &self,
        profile: &SshProfile,
        shell: RemoteShellKind,
    ) -> Result<SshRemoteCapabilities> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        let virtual_root = RemotePath::parse_with_namespace("/", SftpNamespaceKind::Virtual)
            .map_err(DomainError::InvalidConfig)?;
        let mut cache = self.remote_capabilities.lock();
        let cached = cache
            .entry(profile.id.clone())
            .or_insert_with(|| CachedRemoteCapabilities {
                profile: profile.clone(),
                capabilities: SshRemoteCapabilities::default(),
            });
        if cached.profile != *profile {
            *cached = CachedRemoteCapabilities {
                profile: profile.clone(),
                capabilities: SshRemoteCapabilities::default(),
            };
        }
        let capabilities = &mut cached.capabilities;
        capabilities.openssh_client = RemoteCapabilityState::Available;
        capabilities.ssh_authentication = RemoteCapabilityState::Available;
        capabilities.operating_system = RemoteOperatingSystem::Windows;
        capabilities.shell = shell;
        capabilities.terminal = RemoteCapabilityState::Available;
        // 交互终端只证明平台；先建立 Windows 虚拟根，实际目录请求随后验证通道。
        capabilities.sftp = RemoteCapabilityState::Available;
        capabilities.sftp_namespace = SftpNamespaceKind::Virtual;
        capabilities.sftp_canonical_path = Some(virtual_root);
        Ok(capabilities.clone())
    }

    pub async fn execute_diagnostic(
        &self,
        profile_id: &SshProfileId,
        operation: &SshDiagnosticOperation,
        cancellation: DiagnosticCancellation,
    ) -> Result<SshDiagnosticResult> {
        operation.validate().map_err(DomainError::InvalidConfig)?;
        let profile = self.current_profile(profile_id).await?;
        if !profile.production {
            return Err(DomainError::Forbidden(
                "安全诊断只允许生产模式连接使用".into(),
            ));
        }
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        validate_diagnostic_platform(&profile, &capabilities)?;
        let profile_gate = self
            .diagnostic_profiles
            .lock()
            .entry(profile.id.clone())
            .or_insert_with(|| {
                Arc::new(tokio::sync::Semaphore::new(
                    MAX_CONCURRENT_DIAGNOSTICS_PER_PROFILE,
                ))
            })
            .clone();
        let _global = self
            .diagnostic_global
            .clone()
            .try_acquire_owned()
            .map_err(|_| DomainError::Forbidden("全局安全诊断并发已达 4 个上限".into()))?;
        let _profile = profile_gate
            .try_acquire_owned()
            .map_err(|_| DomainError::Forbidden("同一连接同时只能执行 1 个安全诊断".into()))?;
        tracing::info!(
            profile_id = %profile.id,
            operation = operation.kind(),
            platform = ?capabilities.operating_system,
            "ssh diagnostic started"
        );
        let started = Instant::now();
        let result = self
            .driver
            .execute_diagnostic(&profile, &capabilities, operation, cancellation)
            .await;
        match &result {
            Ok(result) => tracing::info!(
                profile_id = %profile.id,
                operation = operation.kind(),
                elapsed_ms = started.elapsed().as_millis(),
                exit_code = ?result.exit_code,
                truncated = result.truncated,
                termination = ?result.termination,
                output_bytes = result.output.len(),
                "ssh diagnostic finished"
            ),
            Err(error) => tracing::warn!(
                profile_id = %profile.id,
                operation = operation.kind(),
                elapsed_ms = started.elapsed().as_millis(),
                error = %error,
                "ssh diagnostic failed"
            ),
        }
        result
    }

    pub async fn terminal_command(
        &self,
        profile_id: &SshProfileId,
        initial_directory: Option<&str>,
    ) -> Result<SshLaunchCommand> {
        let generation = self.terminal_generation_if_allowed(profile_id)?;
        let profile = self
            .storage
            .get_ssh_profile(profile_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("SSH 配置已删除".into()))?;
        profile.validate().map_err(DomainError::InvalidConfig)?;
        if profile.production {
            return Err(production_terminal_forbidden());
        }
        if let Some(path) = initial_directory {
            validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
            RemotePath::parse_server_canonical(path).map_err(DomainError::InvalidConfig)?;
        }
        let mut command = self
            .driver
            .terminal_command(&profile, initial_directory)
            .await?;
        if self.terminal_generation_if_allowed(profile_id)? != generation {
            return Err(production_terminal_forbidden());
        }
        command.authorization_generation = generation;
        Ok(command)
    }

    /// 生产状态切换开始后，立即禁止该配置创建新的完整终端。
    pub fn block_terminal_launches(&self, profile_id: &SshProfileId) {
        let mut policy = self.terminal_policy.lock();
        policy.blocked.insert(profile_id.clone());
        advance_generation(&mut policy, profile_id);
    }

    /// 取消切换时恢复非生产终端能力；持久化生产配置仍会被配置门禁拒绝。
    pub fn unblock_terminal_launches(&self, profile_id: &SshProfileId) {
        let mut policy = self.terminal_policy.lock();
        policy.blocked.remove(profile_id);
        advance_generation(&mut policy, profile_id);
    }

    /// PTY 启动前的最后一道同步检查，防止异步构造命令期间配置发生变化。
    pub fn terminal_launch_is_current(&self, command: &SshLaunchCommand) -> bool {
        let policy = self.terminal_policy.lock();
        !policy.blocked.contains(&command.profile_id)
            && policy
                .generations
                .get(&command.profile_id)
                .copied()
                .unwrap_or_default()
                == command.authorization_generation
    }

    fn terminal_generation_if_allowed(&self, profile_id: &SshProfileId) -> Result<u64> {
        let policy = self.terminal_policy.lock();
        if policy.blocked.contains(profile_id) {
            return Err(production_terminal_forbidden());
        }
        Ok(policy
            .generations
            .get(profile_id)
            .copied()
            .unwrap_or_default())
    }

    pub(super) fn advance_terminal_generation(&self, profile_id: &SshProfileId) {
        advance_generation(&mut self.terminal_policy.lock(), profile_id);
    }

    pub(super) async fn current_profile(&self, profile_id: &SshProfileId) -> Result<SshProfile> {
        let profile = self
            .storage
            .get_ssh_profile(profile_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("SSH 配置已删除".into()))?;
        profile.validate().map_err(DomainError::InvalidConfig)?;
        Ok(profile)
    }

    pub(super) async fn capabilities_for_profile(
        &self,
        profile: &SshProfile,
        force_refresh: bool,
    ) -> Result<SshRemoteCapabilities> {
        if !force_refresh
            && let Some(cached) = self.remote_capabilities.lock().get(&profile.id)
            && cached.profile == *profile
        {
            return Ok(cached.capabilities.clone());
        }
        let mut capabilities = self.driver.probe_remote_capabilities(profile).await?;
        if let Some(cached) = self.remote_capabilities.lock().get(&profile.id)
            && cached.profile == *profile
        {
            if cached.capabilities.operating_system != RemoteOperatingSystem::Unknown
                && capabilities.operating_system == RemoteOperatingSystem::Unknown
            {
                capabilities.operating_system = cached.capabilities.operating_system;
                capabilities.shell = cached.capabilities.shell;
            }
            if cached.capabilities.sftp == RemoteCapabilityState::Available
                && should_keep_bootstrapped_sftp(&cached.capabilities, &capabilities)
            {
                capabilities.sftp = cached.capabilities.sftp;
                capabilities.sftp_namespace = cached.capabilities.sftp_namespace;
                capabilities.sftp_canonical_path = cached.capabilities.sftp_canonical_path.clone();
            }
        }
        tracing::info!(
            profile_id = %profile.id,
            operating_system = ?capabilities.operating_system,
            shell = ?capabilities.shell,
            sftp = ?capabilities.sftp,
            sftp_namespace = ?capabilities.sftp_namespace,
            "ssh remote capabilities probed"
        );
        self.remote_capabilities.lock().insert(
            profile.id.clone(),
            CachedRemoteCapabilities {
                profile: profile.clone(),
                capabilities: capabilities.clone(),
            },
        );
        Ok(capabilities)
    }

    fn profile_with_cached_capabilities(&self, profile: &SshProfile) -> SshProfile {
        self.remote_capabilities
            .lock()
            .get(&profile.id)
            .filter(|cached| cached.profile == *profile)
            .map_or_else(
                || profile.clone(),
                |cached| profile_for_capabilities(profile, &cached.capabilities),
            )
    }

    /// 首次打开工作区只走 SFTP 列目录，不等待平台和终端能力探测。
    pub async fn bootstrap_directory(
        &self,
        profile_id: &SshProfileId,
        requested_path: &str,
    ) -> Result<RemoteDirectory> {
        let profile = self.current_profile(profile_id).await?;
        let effective_profile = self.profile_with_cached_capabilities(&profile);
        validate_remote_path(requested_path).map_err(DomainError::InvalidConfig)?;
        let candidates = bootstrap_directory_candidates(&effective_profile, requested_path);
        let mut fallback = None;
        let mut last_error = None;
        for candidate in candidates {
            match self
                .driver
                .list_directory(&effective_profile, &candidate)
                .await
            {
                Ok(directory) => {
                    let conclusive = directory.path != "/" || !directory.entries.is_empty();
                    if conclusive {
                        self.remember_bootstrapped_sftp(&profile, &directory)?;
                        tracing::info!(
                            profile_id = %profile.id,
                            entries = directory.entries.len(),
                            "ssh sftp bootstrap directory loaded"
                        );
                        return Ok(directory);
                    }
                    fallback = Some(directory);
                }
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(directory) = fallback {
            self.remember_bootstrapped_sftp(&profile, &directory)?;
            tracing::info!(
                profile_id = %profile.id,
                entries = directory.entries.len(),
                "ssh sftp bootstrap directory loaded as empty root"
            );
            return Ok(directory);
        }
        Err(last_error.unwrap_or_else(|| DomainError::ConnectionFailed("SFTP 未返回目录".into())))
    }

    fn remember_bootstrapped_sftp(
        &self,
        profile: &SshProfile,
        directory: &RemoteDirectory,
    ) -> Result<()> {
        let path = &directory.path;
        let virtual_windows_root = path == "/"
            && directory
                .entries
                .iter()
                .any(|entry| is_virtual_windows_path(&entry.path));
        let namespace = if virtual_windows_root || is_virtual_windows_path(path) {
            SftpNamespaceKind::Virtual
        } else {
            infer_sftp_namespace(path)
        };
        let canonical = RemotePath::parse_with_namespace(path, namespace)
            .map_err(DomainError::InvalidConfig)?;
        let mut cache = self.remote_capabilities.lock();
        let capabilities =
            cache
                .entry(profile.id.clone())
                .or_insert_with(|| CachedRemoteCapabilities {
                    profile: profile.clone(),
                    capabilities: SshRemoteCapabilities::default(),
                });
        if capabilities.profile == *profile {
            capabilities.capabilities.sftp = RemoteCapabilityState::Available;
            capabilities.capabilities.sftp_namespace = namespace;
            capabilities.capabilities.sftp_canonical_path = Some(canonical);
        }
        Ok(())
    }

    pub async fn report_terminal_launch_failure(&self, executable: &str) {
        self.driver.report_terminal_launch_failure(executable).await;
    }

    pub async fn list_directory(
        &self,
        profile: &SshProfile,
        path: &str,
    ) -> Result<RemoteDirectory> {
        let profile = self.current_profile(&profile.id).await?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        let path = resolved_remote_path(&capabilities, path)?;
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        self.driver.list_directory(&effective_profile, &path).await
    }

    pub async fn read_file_preview(
        &self,
        profile: &SshProfile,
        path: &str,
    ) -> Result<RemoteFilePreview> {
        let profile = self.current_profile(&profile.id).await?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        let path = resolved_remote_path(&capabilities, path)?;
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        self.driver
            .read_file_preview(&effective_profile, &path)
            .await
    }

    pub async fn read_file_chunk(
        &self,
        profile: &SshProfile,
        path: &str,
        position: RemoteFileChunkPosition,
    ) -> Result<RemoteFileChunk> {
        let profile = self.current_profile(&profile.id).await?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        let path = resolved_remote_path(&capabilities, path)?;
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        self.driver
            .read_file_chunk(&effective_profile, &path, position)
            .await
    }

    pub async fn save_file(
        &self,
        profile: &SshProfile,
        path: &str,
        expected: &[u8],
        contents: &[u8],
    ) -> Result<()> {
        let profile = self.current_profile(&profile.id).await?;
        ensure_sftp_writable(&profile)?;
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        ensure_remote_write_platform(&profile, &capabilities, true)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        let path = resolved_remote_path(&capabilities, path)?;
        if expected.len() > MAX_REMOTE_FILE_PREVIEW_BYTES
            || contents.len() > MAX_REMOTE_FILE_PREVIEW_BYTES
        {
            return Err(DomainError::InvalidConfig(format!(
                "编辑文件不能超过 {} MiB",
                MAX_REMOTE_FILE_PREVIEW_BYTES / 1024 / 1024
            )));
        }
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        self.driver
            .save_file(&effective_profile, &path, expected, contents)
            .await
    }

    pub async fn create_directory(&self, profile: &SshProfile, path: &str) -> Result<()> {
        let profile = self.current_profile(&profile.id).await?;
        ensure_sftp_writable(&profile)?;
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        ensure_remote_write_platform(&profile, &capabilities, false)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        let path = resolved_new_remote_path(&capabilities, path)?;
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        self.driver
            .create_directory(&effective_profile, &path)
            .await
    }

    pub async fn rename(&self, profile: &SshProfile, old_path: &str, new_path: &str) -> Result<()> {
        let profile = self.current_profile(&profile.id).await?;
        ensure_sftp_writable(&profile)?;
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        ensure_remote_write_platform(&profile, &capabilities, false)?;
        validate_remote_path(old_path).map_err(DomainError::InvalidConfig)?;
        validate_remote_path(new_path).map_err(DomainError::InvalidConfig)?;
        let old_path = resolved_remote_path(&capabilities, old_path)?;
        let new_path = resolved_new_remote_path(&capabilities, new_path)?;
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        self.driver
            .rename(&effective_profile, &old_path, &new_path)
            .await
    }

    pub async fn remove(
        &self,
        profile: &SshProfile,
        path: &str,
        kind: RemoteEntryKind,
    ) -> Result<()> {
        let profile = self.current_profile(&profile.id).await?;
        ensure_sftp_writable(&profile)?;
        let capabilities = self.capabilities_for_profile(&profile, false).await?;
        ensure_remote_write_platform(&profile, &capabilities, false)?;
        if capabilities.operating_system == RemoteOperatingSystem::Windows
            && !matches!(kind, RemoteEntryKind::File | RemoteEntryKind::Directory)
        {
            return Err(DomainError::Forbidden(
                "Windows 首版拒绝删除符号链接或未知 reparse point".into(),
            ));
        }
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        let path = resolved_remote_path(&capabilities, path)?;
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        self.driver.remove(&effective_profile, &path, kind).await
    }
}

pub(super) fn profile_for_capabilities(
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
) -> SshProfile {
    let mut effective = profile.clone();
    if effective.remote_platform == RemotePlatformPreference::Auto {
        effective.remote_platform = match capabilities.operating_system {
            RemoteOperatingSystem::Linux => RemotePlatformPreference::Linux,
            RemoteOperatingSystem::Windows => RemotePlatformPreference::Windows,
            RemoteOperatingSystem::Unknown => RemotePlatformPreference::Auto,
        };
    }
    effective
}

fn should_keep_bootstrapped_sftp(
    cached: &SshRemoteCapabilities,
    fresh: &SshRemoteCapabilities,
) -> bool {
    let Some(cached_path) = cached.sftp_canonical_path.as_ref() else {
        return false;
    };
    let Some(fresh_path) = fresh.sftp_canonical_path.as_ref() else {
        return true;
    };
    (cached_path.is_root()
        && cached.sftp_namespace == SftpNamespaceKind::Virtual
        && fresh.sftp_namespace != SftpNamespaceKind::Virtual)
        || (fresh_path.is_root() && !cached_path.is_root())
}

fn is_virtual_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes[3] == b'/'
}

fn bootstrap_directory_candidates(profile: &SshProfile, requested_path: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let is_windows = profile.remote_platform == RemotePlatformPreference::Windows;
    let is_jumpserver = profile.origin == ramag_domain::entities::SshProfileOrigin::JumpServer;
    candidates.push(requested_path.to_string());
    if is_jumpserver {
        // 非生产 Windows JumpServer 由基础设施在目标机启动系统 SFTP 服务端，
        // 首次进入虚拟根目录以列出可访问盘符。
        if is_windows && !profile.production {
            return vec!["/".into()];
        }
        if requested_path != "." {
            candidates.push(".".into());
        }
        candidates.dedup();
        return candidates;
    }
    if is_windows {
        if let Some(account) = remote_account_hint(profile) {
            candidates.extend([
                format!("C:/Users/{account}"),
                format!("/C:/Users/{account}"),
            ]);
        }
        candidates.extend([
            "C:/Users".into(),
            "/C:/Users".into(),
            "C:/".into(),
            "/C:/".into(),
        ]);
    }
    if !candidates.iter().any(|candidate| candidate == ".") {
        candidates.push(".".into());
    }
    candidates.dedup();
    candidates
}

fn remote_account_hint(profile: &SshProfile) -> Option<&str> {
    let account = if profile.origin == ramag_domain::entities::SshProfileOrigin::JumpServer {
        profile.username.split('#').nth(1)
    } else {
        Some(profile.username.as_str())
    }?;
    (!account.is_empty()
        && ramag_domain::entities::validate_remote_name_for_namespace(
            account,
            SftpNamespaceKind::WindowsDrive,
        )
        .is_ok())
    .then_some(account)
}

fn production_terminal_forbidden() -> DomainError {
    DomainError::Forbidden("生产模式禁止启动完整 SSH Terminal".into())
}

fn validate_diagnostic_platform(
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
) -> Result<()> {
    if capabilities.operating_system == RemoteOperatingSystem::Unknown {
        return Err(DomainError::Forbidden(
            "远端平台未知，安全诊断已失败关闭".into(),
        ));
    }
    let mismatch = matches!(
        (profile.remote_platform, capabilities.operating_system),
        (
            RemotePlatformPreference::Linux,
            RemoteOperatingSystem::Windows
        ) | (
            RemotePlatformPreference::Windows,
            RemoteOperatingSystem::Linux
        )
    );
    if mismatch {
        return Err(DomainError::Forbidden(
            "远端平台与配置偏好冲突，安全诊断已失败关闭".into(),
        ));
    }
    if capabilities.diagnostic != RemoteCapabilityState::Available {
        return Err(DomainError::Forbidden(
            capabilities
                .diagnostic_message
                .clone()
                .unwrap_or_else(|| "当前连接的安全诊断能力不可用".into()),
        ));
    }
    Ok(())
}

pub(super) fn ensure_remote_write_platform(
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    replaces_existing_file: bool,
) -> Result<()> {
    if capabilities.sftp != RemoteCapabilityState::Available {
        return Err(DomainError::Forbidden(
            "SFTP 能力未确认，不能执行远端写操作".into(),
        ));
    }
    match capabilities.operating_system {
        RemoteOperatingSystem::Linux => Ok(()),
        RemoteOperatingSystem::Windows if replaces_existing_file => Err(DomainError::Forbidden(
            "Windows SFTP 尚不能保证保留 NTFS ACL，已禁用编辑和覆盖现有文件".into(),
        )),
        RemoteOperatingSystem::Windows => Ok(()),
        RemoteOperatingSystem::Unknown => Err(DomainError::Forbidden(format!(
            "远端平台尚未确认，不能对连接「{}」执行写操作",
            profile.name
        ))),
    }
}

pub(super) fn resolved_remote_path(
    capabilities: &SshRemoteCapabilities,
    path: &str,
) -> Result<String> {
    if capabilities.sftp != RemoteCapabilityState::Available {
        return Err(DomainError::Forbidden(
            "SFTP 能力未确认，不能访问远端路径".into(),
        ));
    }
    if path == "." {
        return capabilities
            .sftp_canonical_path
            .as_ref()
            .map(ToString::to_string)
            .ok_or_else(|| DomainError::Forbidden("SFTP 默认目录尚未规范化".into()));
    }
    RemotePath::parse_with_namespace(path, capabilities.sftp_namespace)
        .map(|path| path.to_string())
        .map_err(DomainError::InvalidConfig)
}

pub(super) fn resolved_new_remote_path(
    capabilities: &SshRemoteCapabilities,
    path: &str,
) -> Result<String> {
    let path = resolved_remote_path(capabilities, path)?;
    if capabilities.operating_system == RemoteOperatingSystem::Windows {
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        validate_remote_name_for_namespace(name, SftpNamespaceKind::WindowsDrive)
            .map_err(DomainError::InvalidConfig)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{RemotePath, SshProfileOrigin};

    #[test]
    fn jumpserver_windows_bootstrap_lists_the_remote_drive_root_first() {
        let mut profile = SshProfile::new("asset", "jump.example.com");
        profile.origin = SshProfileOrigin::JumpServer;
        profile.remote_platform = RemotePlatformPreference::Windows;
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
}
