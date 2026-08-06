//! SSH 工作区能力探测与首次 SFTP 目录加载。

use gpui::{Context, Window};
use ramag_domain::entities::{
    RemoteCapabilityState, RemoteOperatingSystem, RemotePath, SshProfileId, SshRemoteCapabilities,
};

use super::SshView;

impl SshView {
    pub(super) fn connect_workspace(
        &mut self,
        id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.workspace_mut(&id) {
            workspace.connection_started = true;
        }
        let should_start_terminal = self
            .workspace_mut(&id)
            .is_some_and(|workspace| workspace.terminals.is_empty());
        if should_start_terminal {
            self.start_terminal(id.clone(), None, window, cx);
        }
        // SFTP 列目录先行，平台与终端能力在后台探测，避免首屏被慢探测阻塞。
        self.bootstrap_directory(id.clone(), cx);
        self.probe_workspace_capabilities(id, cx);
    }

    pub(super) fn probe_workspace_capabilities(
        &mut self,
        id: SshProfileId,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace_mut(&id) else {
            return;
        };
        workspace.capability_generation = workspace.capability_generation.wrapping_add(1);
        let generation = workspace.capability_generation;
        workspace.capability_loading = true;
        workspace.capability_error = None;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service
                .probe_remote_capabilities(&id)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |this, cx| {
                let refresh_path = {
                    let Some(workspace) = this.workspace_mut(&id) else {
                        return;
                    };
                    if workspace.capability_generation != generation {
                        return;
                    }
                    workspace.capability_loading = false;
                    match result {
                        Ok(capabilities) => {
                            let path = directory_path_after_probe(&workspace.path, &capabilities);
                            workspace.capabilities = Some(capabilities);
                            workspace.capability_error = None;
                            match path {
                                Ok(path) => {
                                    if workspace.path != path {
                                        workspace.entries = Default::default();
                                        workspace.selected_path = None;
                                        workspace.directory_loaded = false;
                                    }
                                    workspace.path = path.clone();
                                    workspace.sftp_error = None;
                                    let empty_windows_root = is_empty_windows_root(
                                        &path,
                                        workspace.entries.is_empty(),
                                        workspace.capabilities.as_ref(),
                                    );
                                    ((!workspace.directory_loaded || empty_windows_root)
                                        && !workspace.sftp_loading)
                                        .then_some(path)
                                }
                                Err(error) => {
                                    if !workspace.directory_loaded && !workspace.sftp_loading {
                                        workspace.sftp_error = Some(error);
                                    }
                                    None
                                }
                            }
                        }
                        Err(error) => {
                            workspace.capability_error = Some(error.clone());
                            if !workspace.directory_loaded && !workspace.sftp_loading {
                                workspace.sftp_error = Some(format!("远端能力探测失败：{error}"));
                            }
                            None
                        }
                    }
                };
                if let Some(path) = refresh_path {
                    this.refresh_directory(id.clone(), Some(path), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

pub(super) fn is_empty_windows_root(
    path: &str,
    entries_empty: bool,
    capabilities: Option<&SshRemoteCapabilities>,
) -> bool {
    path == "/"
        && entries_empty
        && capabilities.is_some_and(|capabilities| {
            capabilities.operating_system == RemoteOperatingSystem::Windows
        })
}

fn directory_path_after_probe(
    current_path: &str,
    capabilities: &SshRemoteCapabilities,
) -> Result<String, String> {
    if capabilities.sftp != RemoteCapabilityState::Available {
        return Err(capabilities
            .diagnostic_message
            .clone()
            .unwrap_or_else(|| "远端 SFTP 不可用".into()));
    }
    let canonical = capabilities
        .sftp_canonical_path
        .as_ref()
        .ok_or_else(|| "SFTP 默认目录尚未规范化".to_string())?;
    if current_path == "." {
        return Ok(canonical.to_string());
    }
    // Windows 根目录由盘符枚举建立；后台拿到账号 Home 后不能覆盖已加载的根。
    if current_path == "/" && capabilities.operating_system == RemoteOperatingSystem::Windows {
        return Ok(current_path.to_string());
    }
    if RemotePath::parse_with_namespace(current_path, capabilities.sftp_namespace).is_ok() {
        return Ok(current_path.to_string());
    }
    if let (Ok(current), true) = (
        RemotePath::parse_server_canonical(current_path),
        canonical.is_root(),
    ) && !current.is_root()
    {
        return Ok(current_path.to_string());
    }
    Ok(canonical.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{RemoteOperatingSystem, SftpNamespaceKind};

    #[test]
    fn windows_probe_replaces_default_but_preserves_the_loaded_drive_root() {
        let capabilities = SshRemoteCapabilities {
            operating_system: RemoteOperatingSystem::Windows,
            sftp: RemoteCapabilityState::Available,
            sftp_namespace: SftpNamespaceKind::WindowsDrive,
            sftp_canonical_path: Some(
                RemotePath::parse_server_canonical("C:/Users/Administrator").unwrap(),
            ),
            ..SshRemoteCapabilities::default()
        };

        assert_eq!(
            directory_path_after_probe(".", &capabilities).unwrap(),
            "C:/Users/Administrator"
        );
        assert_eq!(directory_path_after_probe("/", &capabilities).unwrap(), "/");
        assert_eq!(
            directory_path_after_probe("D:/Data", &capabilities).unwrap(),
            "D:/Data"
        );
        assert!(is_empty_windows_root("/", true, Some(&capabilities)));
        assert!(!is_empty_windows_root("/", false, Some(&capabilities)));
    }
}
