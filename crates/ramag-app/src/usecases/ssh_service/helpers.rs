use std::collections::HashSet;

use ramag_domain::entities::{MAX_SSH_WORKSPACES, SshWorkspacePreference, validate_remote_path};
use ramag_domain::error::{DomainError, Result};

use super::{MAX_TRANSFER_ERROR_BYTES, MAX_WORKSPACE_PREFERENCE_BYTES};

pub(super) fn parse_workspace_preference(json: &str) -> Result<SshWorkspacePreference> {
    if json.len() > MAX_WORKSPACE_PREFERENCE_BYTES {
        return Err(DomainError::InvalidConfig("SSH 工作区恢复数据过大".into()));
    }
    let preference = serde_json::from_str(json)
        .map_err(|error| DomainError::InvalidConfig(format!("SSH 工作区恢复数据无效：{error}")))?;
    normalized_workspace_preference(preference)
}

pub(super) fn normalized_workspace_preference(
    mut preference: SshWorkspacePreference,
) -> Result<SshWorkspacePreference> {
    let mut seen = HashSet::new();
    preference.workspaces.retain(|workspace| {
        seen.insert(workspace.profile_id.clone()) && seen.len() <= MAX_SSH_WORKSPACES
    });
    for workspace in &preference.workspaces {
        validate_remote_path(&workspace.last_remote_path).map_err(DomainError::InvalidConfig)?;
    }
    if preference.active_profile_id.as_ref().is_some_and(|active| {
        !preference
            .workspaces
            .iter()
            .any(|workspace| &workspace.profile_id == active)
    }) {
        preference.active_profile_id = None;
    }
    Ok(preference)
}

pub(super) fn bounded_error(mut message: String) -> String {
    if message.len() <= MAX_TRANSFER_ERROR_BYTES {
        return message;
    }
    let mut end = MAX_TRANSFER_ERROR_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push('…');
    message
}
