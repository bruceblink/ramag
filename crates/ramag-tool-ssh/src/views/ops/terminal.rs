use super::*;

impl SshView {
    pub(crate) fn start_active_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_mode != ViewMode::Workspace {
            return;
        }
        if let Some(id) = self.active_workspace_id.clone() {
            self.start_terminal(id, None, window, cx);
        }
    }

    pub(crate) fn start_terminal_in_directory(
        &mut self,
        id: SshProfileId,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_terminal(id, Some(path), window, cx);
    }

    pub(crate) fn reconnect_terminal(
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

    pub(crate) fn start_terminal(
        &mut self,
        id: SshProfileId,
        initial_directory: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_terminal_request(id, initial_directory, None, window, cx);
    }

    pub(crate) fn start_terminal_request(
        &mut self,
        id: SshProfileId,
        initial_directory: Option<String>,
        reconnect_terminal_id: Option<u64>,
        window: &mut Window,
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
                    Ok(_) => Err(ramag_terminal::TerminalError("终端启动已取消".into())),
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
}
