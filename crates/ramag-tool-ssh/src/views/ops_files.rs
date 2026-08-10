use gpui::{Context, Window};
use ramag_domain::entities::{
    MAX_SSH_PATH_BYTES, RemoteEntry, RemoteEntryKind, RemoteOperatingSystem, RemotePath,
    SftpNamespaceKind, SshProfileId, infer_sftp_namespace, validate_remote_name_for_namespace,
    validate_remote_path,
};

use super::SshView;
use super::model::Notice;
use super::ops_connection::is_empty_windows_root;
use super::render_directory_helpers::{
    RemoteEntryActivation, remote_entry_activation, sort_remote_entries,
};

enum RemoteMutation {
    Create(String),
    Rename { old: String, new: String },
    Remove { path: String, kind: RemoteEntryKind },
}

impl SshView {
    pub(super) fn refresh_active_directory(&mut self, cx: &mut Context<Self>) {
        if self.view_mode != super::model::ViewMode::Workspace {
            return;
        }
        if let Some(id) = self.active_workspace_id.clone() {
            self.refresh_directory(id, None, cx);
        }
    }

    pub(super) fn refresh_directory(
        &mut self,
        id: SshProfileId,
        requested_path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.load_directory(id, requested_path, false, cx);
    }

    pub(super) fn bootstrap_directory(&mut self, id: SshProfileId, cx: &mut Context<Self>) {
        self.load_directory(id, None, true, cx);
    }

    pub(super) fn bootstrap_directory_at(
        &mut self,
        id: SshProfileId,
        path: String,
        cx: &mut Context<Self>,
    ) {
        self.load_directory(id, Some(path), true, cx);
    }

    fn load_directory(
        &mut self,
        id: SshProfileId,
        requested_path: Option<String>,
        bootstrap: bool,
        cx: &mut Context<Self>,
    ) {
        let connection_available = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &id)
            .is_some_and(|workspace| self.profile_connection_available(&workspace.profile));
        if !connection_available {
            self.notice = Some(Notice::error("OpenSSH 不可用，请重新探测或指定路径"));
            cx.notify();
            return;
        }
        let Some(workspace) = self.workspace_mut(&id) else {
            return;
        };
        let path = requested_path.unwrap_or_else(|| workspace.path.clone());
        if !bootstrap
            && path != "."
            && let Some(capabilities) = workspace.capabilities.as_ref()
            && let Err(error) = RemotePath::parse_with_namespace(&path, capabilities.sftp_namespace)
        {
            workspace.sftp_error = Some(format!("远端路径与当前命名空间不匹配：{error}"));
            cx.notify();
            return;
        }
        if workspace.sftp_loading
            && workspace.directory_loading_path.as_deref() == Some(path.as_str())
        {
            return;
        }
        workspace.directory_generation = workspace.directory_generation.wrapping_add(1);
        let generation = workspace.directory_generation;
        workspace.sftp_loading = true;
        workspace.directory_loading_path = Some(path.clone());
        workspace.sftp_error = None;
        let profile = workspace.profile.clone();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = if bootstrap {
                service.bootstrap_directory(&profile.id, &path).await
            } else {
                service.list_directory(&profile, &path).await
            };
            let _ = this.update(cx, |this, cx| {
                let Some(workspace) = this.workspace_mut(&id) else {
                    return;
                };
                if workspace.directory_generation != generation {
                    return;
                }
                workspace.sftp_loading = false;
                workspace.directory_loading_path = None;
                let mut retry_windows_root = false;
                match result {
                    Ok(directory) => {
                        workspace.path = directory.path;
                        workspace.directory_loaded = true;
                        let mut entries = directory.entries;
                        sort_remote_entries(&mut entries);
                        workspace.entries = std::sync::Arc::new(entries);
                        workspace.selected_path = None;
                        workspace.sftp_error = None;
                        retry_windows_root = bootstrap
                            && is_empty_windows_root(
                                &workspace.path,
                                workspace.entries.is_empty(),
                                workspace.capabilities.as_ref(),
                            );
                        this.persist_workspaces(cx);
                    }
                    Err(error) => {
                        workspace.sftp_error = Some(error.to_string());
                    }
                }
                if retry_windows_root {
                    this.refresh_directory(id.clone(), Some("/".into()), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_remote_entry(
        &mut self,
        workspace_id: SshProfileId,
        path: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.workspace_mut(&workspace_id) {
            workspace.selected_path = Some(path);
            cx.notify();
        }
    }

    pub(super) fn activate_remote_entry(
        &mut self,
        workspace_id: SshProfileId,
        entry: RemoteEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match remote_entry_activation(entry.kind) {
            RemoteEntryActivation::OpenDirectory => {
                self.refresh_directory(workspace_id, Some(entry.path), cx);
            }
            RemoteEntryActivation::PreviewFile => {
                self.preview_remote_file(workspace_id, entry, window, cx);
            }
            RemoteEntryActivation::Unsupported => {
                self.notice = Some(Notice::error(match entry.kind {
                    RemoteEntryKind::Symlink => "软链接不支持查看",
                    RemoteEntryKind::Other => "类型不支持查看",
                    RemoteEntryKind::File | RemoteEntryKind::Directory => return,
                }));
                cx.notify();
            }
        }
    }

    pub(super) fn prompt_remote_path(
        &mut self,
        workspace_id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace_mut(&workspace_id) else {
            return;
        };
        let initial = workspace.path.clone();
        let entity = cx.entity();
        let favorites = self
            .path_favorites
            .get(&workspace_id)
            .cloned()
            .unwrap_or_default();
        super::path_dialog::open_remote_path_dialog(
            entity,
            workspace_id,
            initial,
            favorites,
            window,
            cx,
        );
    }

    pub(super) fn prompt_create_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.active_workspace() else {
            return;
        };
        let workspace_id = workspace.profile.id.clone();
        let parent_path = workspace.path.clone();
        let entity = cx.entity();
        ramag_ui::open_bounded_prompt(
            "新建",
            "名称",
            "",
            "新建",
            MAX_SSH_PATH_BYTES,
            move |name, _window, app| {
                entity.update(app, |this, cx| {
                    this.create_directory_named(workspace_id, parent_path, name, cx)
                });
            },
            window,
            cx,
        );
    }

    fn create_directory_named(
        &mut self,
        workspace_id: SshProfileId,
        parent_path: String,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let name = name.trim();
        let path = self
            .workspace_mut(&workspace_id)
            .and_then(|workspace| workspace.capabilities.as_ref())
            .ok_or_else(|| "远端能力尚未探测".to_string())
            .and_then(|capabilities| {
                new_remote_child(
                    &parent_path,
                    name,
                    capabilities.sftp_namespace,
                    capabilities.operating_system,
                )
            });
        let path = match path {
            Ok(path) => path,
            Err(error) => {
                self.notice = Some(Notice::error(error));
                cx.notify();
                return;
            }
        };
        self.run_remote_mutation(workspace_id, RemoteMutation::Create(path), cx);
    }

    pub(super) fn prompt_rename_entry(
        &mut self,
        workspace_id: SshProfileId,
        entry: RemoteEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_entry_is_current(&workspace_id, &entry) {
            self.notice = Some(Notice::error("内容已变化，请刷新"));
            cx.notify();
            return;
        }
        let entity = cx.entity();
        let initial = entry.name.clone();
        ramag_ui::open_bounded_prompt(
            "改名",
            "名称",
            &initial,
            "改名",
            MAX_SSH_PATH_BYTES,
            move |name, _window, app| {
                entity.update(app, |this, cx| {
                    this.rename_selected_to(workspace_id, entry.path, name, cx)
                });
            },
            window,
            cx,
        );
    }

    fn rename_selected_to(
        &mut self,
        workspace_id: SshProfileId,
        old_path: String,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .workspace_mut(&workspace_id)
            .and_then(|workspace| workspace.capabilities.as_ref())
            .ok_or_else(|| "远端能力尚未探测".to_string())
            .and_then(|capabilities| {
                let old = RemotePath::parse_with_namespace(&old_path, capabilities.sftp_namespace)?;
                new_remote_child(
                    old.parent().canonical(),
                    name.trim(),
                    capabilities.sftp_namespace,
                    capabilities.operating_system,
                )
            });
        match path {
            Ok(new_path) => self.run_remote_mutation(
                workspace_id,
                RemoteMutation::Rename {
                    old: old_path,
                    new: new_path,
                },
                cx,
            ),
            Err(error) => {
                self.notice = Some(Notice::error(error));
                cx.notify();
            }
        }
    }

    pub(super) fn request_delete_entry(
        &mut self,
        workspace_id: SshProfileId,
        entry: RemoteEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_entry_is_current(&workspace_id, &entry) {
            self.notice = Some(Notice::error("内容已变化，请刷新"));
            cx.notify();
            return;
        }
        let kind_hint = if entry.kind == RemoteEntryKind::Directory {
            "目录内容将一并删除。"
        } else {
            ""
        };
        let entity = cx.entity();
        ramag_ui::open_confirm(
            "删除？",
            format!("将永久删除「{}」。{}", entry.path, kind_hint),
            "删除",
            true,
            move |_window, app| {
                entity.update(app, |this, cx| {
                    this.run_remote_mutation(
                        workspace_id,
                        RemoteMutation::Remove {
                            path: entry.path,
                            kind: entry.kind,
                        },
                        cx,
                    )
                });
            },
            window,
            cx,
        );
    }

    fn remote_entry_is_current(&self, workspace_id: &SshProfileId, entry: &RemoteEntry) -> bool {
        self.workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == workspace_id)
            .is_some_and(|workspace| {
                workspace
                    .entries
                    .iter()
                    .any(|current| current.path == entry.path && current.kind == entry.kind)
            })
    }

    fn run_remote_mutation(
        &mut self,
        workspace_id: SshProfileId,
        mutation: RemoteMutation,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace_mut(&workspace_id) else {
            return;
        };
        if workspace.operation_busy {
            return;
        }
        workspace.operation_busy = true;
        let profile = workspace.profile.clone();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = match mutation {
                RemoteMutation::Create(path) => service.create_directory(&profile, &path).await,
                RemoteMutation::Rename { old, new } => service.rename(&profile, &old, &new).await,
                RemoteMutation::Remove { path, kind } => {
                    service.remove(&profile, &path, kind).await
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(workspace) = this.workspace_mut(&workspace_id) {
                    workspace.operation_busy = false;
                }
                match result {
                    Ok(()) => {
                        this.notice = None;
                        this.refresh_directory(workspace_id, None, cx);
                    }
                    Err(error) => {
                        this.notice = Some(Notice::error(format!("操作失败：{error}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn new_remote_child(
    parent: &str,
    name: &str,
    namespace: SftpNamespaceKind,
    operating_system: RemoteOperatingSystem,
) -> Result<String, String> {
    let name_namespace = if operating_system == RemoteOperatingSystem::Windows {
        SftpNamespaceKind::WindowsDrive
    } else {
        namespace
    };
    validate_remote_name_for_namespace(name, name_namespace)?;
    RemotePath::parse_with_namespace(parent, namespace)?
        .join_child(name)
        .map(|path| path.to_string())
}

pub(super) fn validate_direct_remote_path(path: &str) -> Result<(), String> {
    validate_remote_path(path)?;
    if path == "." {
        return Ok(());
    }
    RemotePath::parse_with_namespace(path, infer_sftp_namespace(path)).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_remote_path_requires_an_absolute_path_or_default_directory() {
        for path in ["/", "/es01/home/yuansuan", "C:/Users/Admin", "."] {
            assert!(validate_direct_remote_path(path).is_ok(), "{path}");
        }
        for path in ["", "es01/home/yuansuan", "C:relative", "/tmp\nroot"] {
            assert!(validate_direct_remote_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn windows_virtual_root_still_uses_windows_new_name_rules() {
        assert!(
            new_remote_child(
                "/Users/Admin",
                "report.txt",
                SftpNamespaceKind::Virtual,
                RemoteOperatingSystem::Windows,
            )
            .is_ok()
        );
        assert!(
            new_remote_child(
                "/Users/Admin",
                "CON.txt",
                SftpNamespaceKind::Virtual,
                RemoteOperatingSystem::Windows,
            )
            .is_err()
        );
    }
}
