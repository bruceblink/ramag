//! 配置、工作区和终端生命周期操作。

use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Focusable as _, Window};
use ramag_domain::entities::{
    MAX_SSH_TERMINALS_PER_WORKSPACE, MAX_SSH_WORKSPACES, SshPathFavorites, SshProfileId,
    SshWorkspacePreference, SshWorkspaceState,
};
use ramag_terminal::{TerminalCommand, TerminalCore, TerminalView};

use super::SshView;
use super::model::{
    Notice, SshWorkspace, TerminalTab, ViewMode, can_close_terminal, terminal_index_after_close,
};

impl SshView {
    pub(super) fn load_initial_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        self.capability_generation = self.capability_generation.wrapping_add(1);
        let capability_generation = self.capability_generation;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let profiles = service.list_profiles().await;
            let preference = service.load_workspace_preference().await;
            let capability = service.probe(None).await.map_err(|error| error.to_string());
            let _ = this.update_in(async_cx, |this, window, cx| {
                if this.load_generation != generation {
                    return;
                }
                this.loading_profiles = false;
                if this.capability_generation == capability_generation {
                    this.default_capability = Some(capability);
                }
                match profiles {
                    Ok(profiles) => {
                        this.profiles = std::sync::Arc::new(profiles);
                        this.load_error = None;
                    }
                    Err(error) => {
                        this.load_error = Some(format!("配置加载失败：{error}"));
                        cx.notify();
                        return;
                    }
                }
                match preference {
                    Ok(preference) => {
                        this.restore_workspaces(preference);
                        if let Some(id) = this.active_workspace_id.clone() {
                            this.sync_directory_filter(&id, window, cx);
                            this.sync_diagnostic_filter(&id, window, cx);
                            this.connect_workspace(id, window, cx);
                        }
                    }
                    Err(error) => {
                        this.notice = Some(Notice::error(format!("工作区恢复失败：{error}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_workspaces(&mut self, preference: SshWorkspacePreference) {
        let SshWorkspacePreference {
            workspaces,
            active_profile_id,
            path_favorites,
        } = preference;
        self.workspaces.clear();
        self.workspace_resizes.clear();
        for saved in workspaces {
            let Some(profile) = self
                .profiles
                .iter()
                .find(|profile| profile.id == saved.profile_id)
                .cloned()
            else {
                continue;
            };
            self.workspaces
                .push(SshWorkspace::placeholder(profile, saved.last_remote_path));
        }
        self.path_favorites = path_favorites
            .into_iter()
            .filter(|favorite| {
                self.profiles
                    .iter()
                    .any(|profile| profile.id == favorite.profile_id)
            })
            .map(|favorite| (favorite.profile_id, favorite.paths))
            .collect();
        self.active_workspace_id = active_profile_id.filter(|id| {
            self.workspaces
                .iter()
                .any(|workspace| workspace.profile_id() == id)
        });
        if self.active_workspace_id.is_some() {
            self.view_mode = ViewMode::Workspace;
        }
    }

    pub(super) fn has_live_terminals(&self, cx: &gpui::App) -> bool {
        self.workspaces.iter().any(|workspace| {
            workspace.terminals.iter().any(|terminal| {
                let core = terminal.view.read(cx).core();
                !core.is_closed() && core.exit_status().is_none()
            })
        })
    }

    pub(super) fn active_workspace(&self) -> Option<&SshWorkspace> {
        let id = self.active_workspace_id.as_ref()?;
        self.workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == id)
    }

    pub(super) fn workspace_mut(&mut self, id: &SshProfileId) -> Option<&mut SshWorkspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.profile_id() == id)
    }

    pub(super) fn show_manager(&mut self, cx: &mut Context<Self>) {
        self.view_mode = ViewMode::Manager;
        cx.notify();
    }

    pub(super) fn select_workspace(
        &mut self,
        id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .workspaces
            .iter()
            .any(|workspace| workspace.profile_id() == &id)
        {
            let needs_connect = self
                .workspaces
                .iter()
                .find(|workspace| workspace.profile_id() == &id)
                .is_some_and(|workspace| {
                    !workspace.connection_started || workspace.sftp_error.is_some()
                });
            self.active_workspace_id = Some(id.clone());
            self.sync_directory_filter(&id, window, cx);
            self.sync_diagnostic_filter(&id, window, cx);
            self.view_mode = ViewMode::Workspace;
            self.persist_workspaces(cx);
            if needs_connect {
                self.connect_workspace(id, window, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn open_workspace(
        &mut self,
        id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
        else {
            return;
        };
        if !self.profile_connection_available(&profile) {
            self.notice = Some(Notice::error("OpenSSH 不可用，请重新探测或指定路径"));
            cx.notify();
            return;
        }
        if !self
            .workspaces
            .iter()
            .any(|workspace| workspace.profile_id() == &id)
        {
            if self.workspaces.len() >= MAX_SSH_WORKSPACES {
                self.notice = Some(Notice::error(format!(
                    "工作区已达上限（{MAX_SSH_WORKSPACES}）"
                )));
                cx.notify();
                return;
            }
            self.workspaces.push(SshWorkspace::placeholder(
                profile.clone(),
                profile.initial_path().to_string(),
            ));
        }
        self.active_workspace_id = Some(id.clone());
        self.sync_directory_filter(&id, window, cx);
        self.sync_diagnostic_filter(&id, window, cx);
        self.view_mode = ViewMode::Workspace;
        self.persist_workspaces(cx);
        self.connect_workspace(id, window, cx);
        cx.notify();
    }

    fn sync_directory_filter(
        &mut self,
        id: &SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == id)
            .map(|workspace| workspace.directory_query.clone())
            .unwrap_or_default();
        self.directory_search.update(cx, |state, cx| {
            state.set_value(query, window, cx);
        });
    }

    fn sync_diagnostic_filter(
        &mut self,
        id: &SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == id)
            .map(|workspace| workspace.diagnostic_query.clone())
            .unwrap_or_default();
        self.diagnostic_search.update(cx, |state, cx| {
            state.set_value(query, window, cx);
        });
    }

    pub(super) fn connect_active_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_mode != ViewMode::Workspace {
            return;
        }
        if let Some(id) = self.active_workspace_id.clone() {
            self.connect_workspace(id, window, cx);
        }
    }

    pub(super) fn start_active_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_mode != ViewMode::Workspace {
            return;
        }
        if let Some(id) = self.active_workspace_id.clone() {
            self.start_terminal(id, None, window, cx);
        }
    }

    pub(super) fn start_terminal_in_directory(
        &mut self,
        id: SshProfileId,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_terminal(id, Some(path), window, cx);
    }

    pub(super) fn reconnect_terminal(
        &mut self,
        workspace_id: SshProfileId,
        terminal_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let exited = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &workspace_id)
            .and_then(|workspace| {
                workspace
                    .terminals
                    .iter()
                    .find(|terminal| terminal.id == terminal_id)
            })
            .is_some_and(|terminal| terminal.view.read(cx).core().exit_status().is_some());
        if self.view_mode == ViewMode::Workspace && exited {
            // 重连必须复用当前标签；失败时保留旧视图，成功后只替换其 PTY。
            self.start_terminal_request(workspace_id, None, Some(terminal_id), window, cx);
        }
    }

    pub(super) fn start_terminal(
        &mut self,
        id: SshProfileId,
        initial_directory: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_terminal_request(id, initial_directory, None, window, cx);
    }

    fn start_terminal_request(
        &mut self,
        id: SshProfileId,
        initial_directory: Option<String>,
        reconnect_terminal_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &id)
            .is_some_and(|workspace| workspace.profile.production)
        {
            self.notice = Some(Notice::error("生产模式禁止完整终端，请使用低影响只读诊断"));
            cx.notify();
            return;
        }
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
        if reconnect_terminal_id.is_none()
            && workspace.terminals.len() >= MAX_SSH_TERMINALS_PER_WORKSPACE
        {
            self.notice = Some(Notice::error(format!(
                "终端已满（最多同时 {MAX_SSH_TERMINALS_PER_WORKSPACE} 个）"
            )));
            cx.notify();
            return;
        }
        if workspace.terminal_loading {
            return;
        }
        workspace.terminal_loading = true;
        workspace.terminal_generation = workspace.terminal_generation.wrapping_add(1);
        let generation = workspace.terminal_generation;
        let profile_id = workspace.profile.id.clone();
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let command = service
                .terminal_command(&profile_id, initial_directory.as_deref())
                .await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                let new_terminal_id = this.next_terminal_id;
                let Some(workspace) = this.workspace_mut(&id) else {
                    return;
                };
                if workspace.terminal_generation != generation {
                    return;
                }
                workspace.terminal_loading = false;
                let result = match command {
                    Ok(command) if service.terminal_launch_is_current(&command) => {
                        let executable = command.program.clone();
                        let mut terminal_command =
                            TerminalCommand::new(command.program, command.args);
                        terminal_command.env = command.env;
                        let result = TerminalCore::start(terminal_command);
                        if result.is_err() {
                            let service = service.clone();
                            cx.spawn(async move |_, _| {
                                service.report_terminal_launch_failure(&executable).await;
                            })
                            .detach();
                        }
                        result
                    }
                    Ok(_) => Err(ramag_terminal::TerminalError(
                        "终端启动已因生产状态变化而取消".into(),
                    )),
                    Err(error) => Err(ramag_terminal::TerminalError(error.to_string())),
                };
                match result {
                    Ok(core) => {
                        let shell =
                            workspace.capabilities.as_ref().map_or(
                                ramag_domain::entities::RemoteShellKind::Unknown,
                                |value| value.shell,
                            );
                        let terminal: Entity<TerminalView> =
                            cx.new(|cx| TerminalView::new(core, window, cx));
                        let terminal_for_focus = terminal.clone();
                        let terminal_id = reconnect_terminal_id.unwrap_or(new_terminal_id);
                        let replaced = if let Some(target_id) = reconnect_terminal_id {
                            let Some(existing) = workspace
                                .terminals
                                .iter_mut()
                                .find(|terminal| terminal.id == target_id)
                            else {
                                return;
                            };
                            existing.view = terminal;
                            true
                        } else {
                            let label = workspace.next_terminal_label();
                            workspace.terminals.push(TerminalTab {
                                id: terminal_id,
                                label,
                                view: terminal,
                            });
                            false
                        };
                        workspace.active_terminal_id = Some(terminal_id);
                        tracing::info!(
                            profile_id = %id,
                            terminal_id,
                            reconnect = replaced,
                            "ssh terminal session ready"
                        );
                        if !replaced {
                            this.next_terminal_id = this.next_terminal_id.wrapping_add(1).max(1);
                        }
                        this.notice = None;
                        if this.view_mode == ViewMode::Workspace
                            && this.active_workspace_id.as_ref() == Some(&id)
                        {
                            terminal_for_focus
                                .read(cx)
                                .focus_handle(cx)
                                .focus(window, cx);
                        }
                        match initial_directory.clone() {
                            Some(path) => this.enter_terminal_directory_when_ready(
                                id.clone(),
                                terminal_id,
                                path,
                                shell,
                                window,
                                cx,
                            ),
                            None => this.load_windows_directory_from_terminal_when_ready(
                                id.clone(),
                                terminal_id,
                                window,
                                cx,
                            ),
                        }
                    }
                    Err(error) => {
                        this.notice = Some(Notice::error(format!("终端启动失败：{error}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_terminal(
        &mut self,
        workspace_id: SshProfileId,
        terminal_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = self.workspace_mut(&workspace_id).and_then(|workspace| {
            let terminal = workspace
                .terminals
                .iter()
                .find(|terminal| terminal.id == terminal_id)
                .map(|terminal| terminal.view.clone())?;
            workspace.active_terminal_id = Some(terminal_id);
            Some(terminal)
        });
        if let Some(terminal) = terminal {
            terminal.read(cx).focus_handle(cx).focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn close_active_terminal_or_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view_mode != ViewMode::Workspace {
            return;
        }
        let Some(workspace_id) = self.active_workspace_id.clone() else {
            return;
        };
        let terminal_id = self
            .workspace_mut(&workspace_id)
            .and_then(|workspace| workspace.active_terminal_id);
        if let Some(terminal_id) = terminal_id {
            self.close_terminal(workspace_id, terminal_id, window, cx);
        } else {
            self.request_close_workspace(workspace_id, window, cx);
        }
    }

    pub(super) fn close_terminal(
        &mut self,
        workspace_id: SshProfileId,
        terminal_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next_terminal = {
            let Some(workspace) = self.workspace_mut(&workspace_id) else {
                return;
            };
            if !can_close_terminal(workspace.terminals.len()) {
                return;
            }
            let Some(index) = workspace
                .terminals
                .iter()
                .position(|terminal| terminal.id == terminal_id)
            else {
                return;
            };
            workspace.terminals.remove(index);
            if workspace.active_terminal_id == Some(terminal_id) {
                let terminal = terminal_index_after_close(index, workspace.terminals.len())
                    .and_then(|index| workspace.terminals.get(index));
                workspace.active_terminal_id = terminal.map(|terminal| terminal.id);
                terminal.map(|terminal| terminal.view.clone())
            } else {
                None
            }
        };
        tracing::info!(
            profile_id = %workspace_id,
            terminal_id,
            "ssh terminal closed"
        );
        if let Some(terminal) = next_terminal {
            terminal.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn request_close_workspace(
        &mut self,
        id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let has_transfer = self
            .service
            .transfer_tasks()
            .iter()
            .any(|task| task.profile_id == id && !task.status.is_terminal());
        if !has_transfer {
            self.close_workspace(id, window, cx);
            return;
        }
        let entity = cx.entity();
        ramag_ui::open_confirm(
            "关闭？",
            "关闭会取消传输并断开 SSH。",
            "关闭",
            true,
            move |window, app| {
                entity.update(app, |this, cx| this.close_workspace(id, window, cx));
            },
            window,
            cx,
        );
    }

    fn close_workspace(&mut self, id: SshProfileId, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces
            .retain(|workspace| workspace.profile_id() != &id);
        self.workspace_resizes.remove(&id);
        if self.active_workspace_id.as_ref() == Some(&id) {
            self.active_workspace_id = self
                .workspaces
                .first()
                .map(|workspace| workspace.profile.id.clone());
        }
        if self.active_workspace_id.is_none() {
            self.view_mode = ViewMode::Manager;
            self.directory_search.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
            self.diagnostic_search.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        } else if let Some(active_id) = self.active_workspace_id.clone() {
            self.sync_directory_filter(&active_id, window, cx);
            self.sync_diagnostic_filter(&active_id, window, cx);
        }
        self.persist_workspaces(cx);
        let service = self.service.clone();
        cx.spawn(async move |_, _| {
            if let Err(error) = service.disconnect(&id).await {
                tracing::warn!(error = %error, profile_id = %id, "close ssh workspace failed");
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn persist_workspaces(&mut self, cx: &mut Context<Self>) {
        self.persist_generation = self.persist_generation.wrapping_add(1);
        let generation = self.persist_generation;
        let mut path_favorites = self
            .path_favorites
            .iter()
            .filter(|(_, paths)| !paths.is_empty())
            .map(|(profile_id, paths)| SshPathFavorites {
                profile_id: profile_id.clone(),
                paths: paths.clone(),
            })
            .collect::<Vec<_>>();
        path_favorites.sort_by(|left, right| {
            left.profile_id
                .to_string()
                .cmp(&right.profile_id.to_string())
        });
        let preference = SshWorkspacePreference {
            workspaces: self
                .workspaces
                .iter()
                .map(|workspace| SshWorkspaceState {
                    profile_id: workspace.profile.id.clone(),
                    last_remote_path: workspace.path.clone(),
                })
                .collect(),
            active_profile_id: self.active_workspace_id.clone(),
            path_favorites,
        };
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let current = this
                .update(cx, |this, _| this.persist_generation == generation)
                .unwrap_or(false);
            if current && let Err(error) = service.save_workspace_preference(&preference).await {
                tracing::warn!(error = %error, "persist ssh workspaces failed");
            }
        })
        .detach();
    }
}
