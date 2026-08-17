use super::*;

impl SshService {
    pub async fn terminal_command(
        &self,
        profile_id: &SshProfileId,
        initial_directory: Option<&str>,
    ) -> Result<SshLaunchCommand> {
        let result = async {
            let generation = self.terminal_generation_if_allowed(profile_id)?;
            let profile = self
                .storage
                .get_ssh_profile(profile_id)
                .await?
                .ok_or_else(|| DomainError::NotFound("SSH 配置已删除".into()))?;
            profile.validate().map_err(DomainError::InvalidConfig)?;
            if let Some(path) = initial_directory {
                validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
                RemotePath::parse_server_canonical(path).map_err(DomainError::InvalidConfig)?;
            }
            let mut command = self
                .driver
                .terminal_command(&profile, initial_directory)
                .await?;
            if self.terminal_generation_if_allowed(profile_id)? != generation {
                return Err(terminal_launch_cancelled());
            }
            command.authorization_generation = generation;
            Ok(command)
        }
        .await;
        if let Err(error) = &result {
            tracing::warn!(
                operation = "ssh_terminal_command",
                error = %error,
                profile_id = %profile_id,
                initial_directory_configured = initial_directory.is_some(),
                "prepare ssh terminal command failed"
            );
        }
        result
    }

    /// 生命周期操作期间禁止创建终端。
    pub fn block_terminal_launches(&self, profile_id: &SshProfileId) {
        let mut policy = self.terminal_policy.lock();
        policy.blocked.insert(profile_id.clone());
        advance_generation(&mut policy, profile_id);
    }

    /// 取消生命周期操作后恢复终端创建。
    pub fn unblock_terminal_launches(&self, profile_id: &SshProfileId) {
        let mut policy = self.terminal_policy.lock();
        policy.blocked.remove(profile_id);
        advance_generation(&mut policy, profile_id);
    }

    /// PTY 启动前确认命令仍属于当前配置版本。
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
            return Err(terminal_launch_cancelled());
        }
        Ok(policy
            .generations
            .get(profile_id)
            .copied()
            .unwrap_or_default())
    }

    pub(in super::super) fn advance_terminal_generation(&self, profile_id: &SshProfileId) {
        advance_generation(&mut self.terminal_policy.lock(), profile_id);
    }
}

fn terminal_launch_cancelled() -> DomainError {
    DomainError::Forbidden("SSH 终端启动已取消".into())
}
