//! SSH 远端目录写操作。

use super::*;

impl SshService {
    pub async fn create_directory(&self, profile: &SshProfile, path: &str) -> Result<()> {
        let profile_id = profile.id.clone();
        let result = async {
            let profile = self.current_profile(&profile_id).await?;
            ensure_sftp_writable(&profile)?;
            let capabilities = self.capabilities_for_profile(&profile, false).await?;
            ensure_remote_write_platform(&profile, &capabilities)?;
            validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
            let path = resolved_new_remote_path(&capabilities, path)?;
            let effective_profile = profile_for_capabilities(&profile, &capabilities);
            self.driver
                .create_directory(&effective_profile, &path)
                .await
        }
        .await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "ssh_directory_create",
                profile_id = %profile_id,
                path = ?path,
                "ssh directory created"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_directory_create",
                error = %error,
                profile_id = %profile_id,
                path = ?path,
                "create ssh directory failed"
            ),
        }
        result
    }

    pub async fn rename(&self, profile: &SshProfile, old_path: &str, new_path: &str) -> Result<()> {
        let profile_id = profile.id.clone();
        let result = async {
            let profile = self.current_profile(&profile_id).await?;
            ensure_sftp_writable(&profile)?;
            let capabilities = self.capabilities_for_profile(&profile, false).await?;
            ensure_remote_write_platform(&profile, &capabilities)?;
            validate_remote_path(old_path).map_err(DomainError::InvalidConfig)?;
            validate_remote_path(new_path).map_err(DomainError::InvalidConfig)?;
            let old_path = resolved_remote_path(&capabilities, old_path)?;
            let new_path = resolved_new_remote_path(&capabilities, new_path)?;
            let effective_profile = profile_for_capabilities(&profile, &capabilities);
            self.driver
                .rename(&effective_profile, &old_path, &new_path)
                .await
        }
        .await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "ssh_path_rename",
                profile_id = %profile_id,
                old_path = ?old_path,
                new_path = ?new_path,
                "ssh path renamed"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_path_rename",
                error = %error,
                profile_id = %profile_id,
                old_path = ?old_path,
                new_path = ?new_path,
                "rename ssh path failed"
            ),
        }
        result
    }

    pub async fn remove(
        &self,
        profile: &SshProfile,
        path: &str,
        kind: RemoteEntryKind,
    ) -> Result<()> {
        let profile_id = profile.id.clone();
        let result = async {
            let profile = self.current_profile(&profile_id).await?;
            ensure_sftp_writable(&profile)?;
            let capabilities = self.capabilities_for_profile(&profile, false).await?;
            ensure_remote_write_platform(&profile, &capabilities)?;
            validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
            let path = resolved_remote_path(&capabilities, path)?;
            let effective_profile = profile_for_capabilities(&profile, &capabilities);
            self.driver.remove(&effective_profile, &path, kind).await
        }
        .await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "ssh_remote_remove",
                profile_id = %profile_id,
                path = ?path,
                kind = ?kind,
                "ssh remote entry removed"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_remote_remove",
                error = %error,
                profile_id = %profile_id,
                path = ?path,
                kind = ?kind,
                "remove ssh remote entry failed"
            ),
        }
        result
    }
}
