use super::*;

impl SshService {
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

    pub(super) fn remember_bootstrapped_sftp(
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
        let result = self.driver.list_directory(&effective_profile, &path).await;
        match &result {
            Ok(directory) => {
                tracing::debug!(profile_id = %profile.id, path = ?path, entries = directory.entries.len(), "ssh directory listed")
            }
            Err(error) => {
                tracing::warn!(error = %error, profile_id = %profile.id, path = ?path, "list ssh directory failed")
            }
        }
        result
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
        let result = self
            .driver
            .read_file_preview(&effective_profile, &path)
            .await;
        match &result {
            Ok(preview) => {
                tracing::debug!(profile_id = %profile.id, path = ?path, bytes = preview.bytes.len(), total_bytes = preview.total_bytes, truncated = preview.truncated, "ssh file preview loaded")
            }
            Err(error) => {
                tracing::warn!(error = %error, profile_id = %profile.id, path = ?path, "load ssh file preview failed")
            }
        }
        result
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
        let result = self
            .driver
            .read_file_chunk(&effective_profile, &path, position)
            .await;
        match &result {
            Ok(chunk) => {
                tracing::debug!(profile_id = %profile.id, path = ?path, bytes = chunk.bytes.len(), offset = chunk.offset, total_bytes = chunk.total_bytes, "ssh file chunk loaded")
            }
            Err(error) => {
                tracing::warn!(error = %error, profile_id = %profile.id, path = ?path, position = ?position, "load ssh file chunk failed")
            }
        }
        result
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
        ensure_remote_write_platform(&profile, &capabilities)?;
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
        let result = self
            .driver
            .save_file(&effective_profile, &path, expected, contents)
            .await;
        match &result {
            Ok(()) => {
                tracing::info!(profile_id = %profile.id, path = ?path, previous_bytes = expected.len(), bytes = contents.len(), "ssh file saved")
            }
            Err(error) => {
                tracing::warn!(error = %error, profile_id = %profile.id, path = ?path, previous_bytes = expected.len(), bytes = contents.len(), "save ssh file failed")
            }
        }
        result
    }
}
