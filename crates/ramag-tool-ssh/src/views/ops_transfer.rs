//! SFTP 上传、下载与传输队列操作。

use std::path::PathBuf;

use gpui::{Context, Window};
use ramag_domain::entities::{
    OverwritePolicy, RemoteEntry, SshProfile, SshProfileId, TransferDirection, TransferId,
    join_remote_path,
};

use super::SshView;
use super::model::Notice;

impl SshView {
    pub(super) fn pick_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.active_workspace() else {
            return;
        };
        let workspace_id = workspace.profile.id.clone();
        let target_directory = workspace.path.clone();
        let visible_entries = workspace.entries.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let picked = rfd::AsyncFileDialog::new().pick_file().await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                let Some(handle) = picked else {
                    return;
                };
                let local_path = handle.path().to_path_buf();
                let Some(name) = local_path.file_name().and_then(|name| name.to_str()) else {
                    this.notice = Some(Notice::error("文件名不是 UTF-8"));
                    cx.notify();
                    return;
                };
                if !this
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.profile_id() == &workspace_id)
                {
                    return;
                }
                let remote_path = match join_remote_path(&target_directory, name) {
                    Ok(path) => path,
                    Err(error) => {
                        this.notice = Some(Notice::error(error));
                        cx.notify();
                        return;
                    }
                };
                let exists = visible_entries
                    .iter()
                    .any(|entry| entry.path == remote_path);
                if exists {
                    let entity = cx.entity();
                    ramag_ui::open_confirm(
                        "覆盖？",
                        format!("「{remote_path}」已存在，将被替换。"),
                        "覆盖",
                        true,
                        move |_window, app| {
                            entity.update(app, |this, cx| {
                                this.begin_upload(
                                    workspace_id,
                                    local_path,
                                    remote_path,
                                    OverwritePolicy::Overwrite,
                                    cx,
                                )
                            });
                        },
                        window,
                        cx,
                    );
                } else {
                    this.begin_upload(
                        workspace_id,
                        local_path,
                        remote_path,
                        OverwritePolicy::Refuse,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    pub(super) fn pick_download(
        &mut self,
        workspace_id: SshProfileId,
        entry: RemoteEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let archive = entry.kind == ramag_domain::entities::RemoteEntryKind::Directory;
        let file_name = if archive {
            format!("{}.tar.gz", entry.name)
        } else {
            entry.name.clone()
        };
        cx.spawn_in(window, async move |this, async_cx| {
            let picked = rfd::AsyncFileDialog::new()
                .set_file_name(&file_name)
                .save_file()
                .await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                let Some(handle) = picked else {
                    return;
                };
                if !this
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.profile_id() == &workspace_id)
                {
                    return;
                }
                let local_path = handle.path().to_path_buf();
                if local_path.exists() {
                    let entity = cx.entity();
                    let display = local_path.display().to_string();
                    ramag_ui::open_confirm(
                        "覆盖？",
                        format!("「{display}」已存在，将被替换。"),
                        "覆盖",
                        true,
                        move |_window, app| {
                            entity.update(app, |this, cx| {
                                this.begin_download(
                                    workspace_id,
                                    entry.path,
                                    local_path,
                                    OverwritePolicy::Overwrite,
                                    archive,
                                    cx,
                                )
                            });
                        },
                        window,
                        cx,
                    );
                } else {
                    this.begin_download(
                        workspace_id,
                        entry.path,
                        local_path,
                        OverwritePolicy::Refuse,
                        archive,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn begin_upload(
        &mut self,
        workspace_id: SshProfileId,
        local_path: PathBuf,
        remote_path: String,
        overwrite: OverwritePolicy,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.profile_for_workspace(&workspace_id) else {
            return;
        };
        match self
            .service
            .enqueue_upload(&profile, &local_path, &remote_path)
        {
            Ok(id) => self.execute_transfer(id, profile, TransferDirection::Upload, overwrite, cx),
            Err(error) => {
                self.notice = Some(Notice::error(format!("上传失败：{error}")));
                cx.notify();
            }
        }
    }

    fn begin_download(
        &mut self,
        workspace_id: SshProfileId,
        remote_path: String,
        local_path: PathBuf,
        overwrite: OverwritePolicy,
        archive: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.profile_for_workspace(&workspace_id) else {
            return;
        };
        let enqueue = if archive {
            self.service
                .enqueue_directory_download(&profile, &remote_path, &local_path)
        } else {
            self.service
                .enqueue_download(&profile, &remote_path, &local_path)
        };
        match enqueue {
            Ok(id) => {
                let direction = if archive {
                    TransferDirection::DownloadArchive
                } else {
                    TransferDirection::Download
                };
                self.execute_transfer(id, profile, direction, overwrite, cx)
            }
            Err(error) => {
                self.notice = Some(Notice::error(format!("下载失败：{error}")));
                cx.notify();
            }
        }
    }

    fn profile_for_workspace(&self, id: &SshProfileId) -> Option<SshProfile> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == id)
            .map(|workspace| workspace.profile.clone())
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .cloned()
            })
    }

    fn execute_transfer(
        &mut self,
        id: TransferId,
        profile: SshProfile,
        direction: TransferDirection,
        overwrite: OverwritePolicy,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.workspace_mut(&profile.id) {
            workspace.transfers_visible = true;
        }
        cx.notify();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.execute_transfer(&id, overwrite).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.notice = Some(Notice::info(match direction {
                            TransferDirection::Upload => "已上传",
                            TransferDirection::Download | TransferDirection::DownloadArchive => {
                                "已下载"
                            }
                        }));
                        if direction == TransferDirection::Upload {
                            this.refresh_directory(profile.id.clone(), None, cx);
                        }
                    }
                    Err(error) => {
                        this.notice = Some(Notice::error(format!("传输失败：{error}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn cancel_transfer(&mut self, id: TransferId, cx: &mut Context<Self>) {
        if self.service.cancel_transfer(&id) {
            self.notice = Some(Notice::info("取消中…"));
            cx.notify();
        }
    }

    pub(super) fn retry_transfer(
        &mut self,
        id: TransferId,
        overwrite: OverwritePolicy,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self
            .service
            .transfer_tasks()
            .into_iter()
            .find(|task| task.id == id)
        else {
            return;
        };
        let Some(profile) = self.profile_for_workspace(&task.profile_id) else {
            self.notice = Some(Notice::error("SSH 配置已删除"));
            cx.notify();
            return;
        };
        match self.service.retry_transfer(&id) {
            Ok(new_id) => self.execute_transfer(new_id, profile, task.direction, overwrite, cx),
            Err(error) => {
                self.notice = Some(Notice::error(format!("重试失败：{error}")));
                cx.notify();
            }
        }
    }

    pub(super) fn confirm_overwrite_retry(
        &mut self,
        id: TransferId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        ramag_ui::open_confirm(
            "覆盖？",
            "目标已存在，将被替换。",
            "覆盖",
            true,
            move |_window, app| {
                entity.update(app, |this, cx| {
                    this.retry_transfer(id, OverwritePolicy::Overwrite, cx)
                });
            },
            window,
            cx,
        );
    }

    pub(super) fn clear_finished_transfers(&mut self, cx: &mut Context<Self>) {
        self.service.clear_finished_transfers();
        let Some(workspace_id) = self.active_workspace_id.clone() else {
            cx.notify();
            return;
        };
        let has_tasks = self
            .service
            .transfer_tasks()
            .iter()
            .any(|task| task.profile_id == workspace_id);
        if !has_tasks && let Some(workspace) = self.workspace_mut(&workspace_id) {
            workspace.transfers_visible = false;
        }
        cx.notify();
    }

    pub(super) fn toggle_transfer_panel(&mut self, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.active_workspace_id.clone() else {
            return;
        };
        let has_tasks = self
            .service
            .transfer_tasks()
            .iter()
            .any(|task| task.profile_id == workspace_id);
        if !has_tasks {
            return;
        }
        if let Some(workspace) = self.workspace_mut(&workspace_id) {
            workspace.transfers_visible = !workspace.transfers_visible;
            cx.notify();
        }
    }

    pub(super) fn hide_transfer_panel(&mut self, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.active_workspace_id.clone() else {
            return;
        };
        if let Some(workspace) = self.workspace_mut(&workspace_id) {
            workspace.transfers_visible = false;
            cx.notify();
        }
    }
}
