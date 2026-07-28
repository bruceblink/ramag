//! SFTP 导航与远程文件操作。

use gpui::{Context, Window};
use ramag_domain::entities::{
    MAX_SSH_PATH_BYTES, RemoteEntry, RemoteEntryKind, SshProfileId, join_remote_path,
    parent_remote_path, validate_remote_name,
};

use super::SshView;
use super::model::Notice;

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
        let connection_available = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &id)
            .is_some_and(|workspace| self.profile_connection_available(&workspace.profile));
        if !connection_available {
            self.notice = Some(Notice::error(
                "OpenSSH 当前不可用；请先在连接管理中重新探测，或配置自定义绝对路径",
            ));
            cx.notify();
            return;
        }
        let Some(workspace) = self.workspace_mut(&id) else {
            return;
        };
        workspace.directory_generation = workspace.directory_generation.wrapping_add(1);
        let generation = workspace.directory_generation;
        workspace.sftp_loading = true;
        workspace.sftp_error = None;
        let path = requested_path.unwrap_or_else(|| workspace.path.clone());
        let profile = workspace.profile.clone();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.list_directory(&profile, &path).await;
            let _ = this.update(cx, |this, cx| {
                let Some(workspace) = this.workspace_mut(&id) else {
                    return;
                };
                if workspace.directory_generation != generation {
                    return;
                }
                workspace.sftp_loading = false;
                match result {
                    Ok(directory) => {
                        workspace.path = directory.path;
                        workspace.entries = std::sync::Arc::new(directory.entries);
                        workspace.selected_path = None;
                        workspace.sftp_error = None;
                        this.persist_workspaces(cx);
                    }
                    Err(error) => {
                        workspace.sftp_error = Some(error.to_string());
                    }
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
        match entry.kind {
            RemoteEntryKind::Directory => {
                self.refresh_directory(workspace_id, Some(entry.path), cx);
            }
            RemoteEntryKind::File => {
                self.pick_download(workspace_id, entry, window, cx);
            }
            RemoteEntryKind::Symlink => {
                self.notice = Some(Notice::error(
                    "首个版本不跟随或下载远程软链接，请选择普通文件",
                ));
                cx.notify();
            }
            RemoteEntryKind::Other => {
                self.notice = Some(Notice::error("该远程条目类型不支持下载"));
                cx.notify();
            }
        }
    }

    pub(super) fn selected_entry(&self, workspace_id: &SshProfileId) -> Option<RemoteEntry> {
        let workspace = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == workspace_id)?;
        let selected = workspace.selected_path.as_ref()?;
        workspace
            .entries
            .iter()
            .find(|entry| &entry.path == selected)
            .cloned()
    }

    pub(super) fn prompt_create_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.active_workspace() else {
            return;
        };
        let workspace_id = workspace.profile.id.clone();
        let parent_path = workspace.path.clone();
        let entity = cx.entity();
        ramag_ui::open_bounded_prompt(
            "新建目录",
            "目录名称",
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
        if let Err(error) = validate_remote_name(name.trim()) {
            self.notice = Some(Notice::error(error));
            cx.notify();
            return;
        }
        match join_remote_path(&parent_path, name.trim()) {
            Ok(path) => self.run_remote_mutation(workspace_id, RemoteMutation::Create(path), cx),
            Err(error) => {
                self.notice = Some(Notice::error(error));
                cx.notify();
            }
        }
    }

    pub(super) fn prompt_rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.active_workspace_id.clone() else {
            return;
        };
        let Some(entry) = self.selected_entry(&workspace_id) else {
            return;
        };
        let entity = cx.entity();
        let initial = entry.name.clone();
        ramag_ui::open_bounded_prompt(
            "改名",
            "新名称",
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
        if let Err(error) = validate_remote_name(name.trim()) {
            self.notice = Some(Notice::error(error));
            cx.notify();
            return;
        }
        let parent_path = match parent_remote_path(&old_path) {
            Ok(path) => path,
            Err(error) => {
                self.notice = Some(Notice::error(error));
                cx.notify();
                return;
            }
        };
        match join_remote_path(&parent_path, name.trim()) {
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

    pub(super) fn request_delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.active_workspace_id.clone() else {
            return;
        };
        let Some(entry) = self.selected_entry(&workspace_id) else {
            return;
        };
        let kind_hint = if entry.kind == RemoteEntryKind::Directory {
            "目录内的全部内容也会递归删除；软链接不会被跟随。"
        } else {
            ""
        };
        let entity = cx.entity();
        ramag_ui::open_confirm(
            "确认删除？",
            format!("「{}」将被永久删除且无法恢复。{}", entry.path, kind_hint),
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
                        this.notice = Some(Notice::error(format!("远程文件操作失败：{error}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
