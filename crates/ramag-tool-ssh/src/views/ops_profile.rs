//! SSH 连接列表、配置弹窗与删除操作。

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Entity, ParentElement, Styled, Window, px};
use gpui_component::WindowExt as _;
use ramag_domain::entities::{JumpServerRdpSession, SshProfile, SshProfileId};

use super::SshView;
use super::jumpserver_dialog::{JumpServerEvent, JumpServerPanel};
use super::model::Notice;
use super::profile_dialog::{ProfileFormEvent, SshProfileFormPanel};

impl SshView {
    pub(super) fn open_profile_rdp(
        &mut self,
        profile_id: SshProfileId,
        session: JumpServerRdpSession,
        cx: &mut Context<Self>,
    ) {
        if self.opening_rdp_profile.is_some() {
            return;
        }
        self.opening_rdp_profile = Some(profile_id.clone());
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = match service
                .create_saved_jumpserver_rdp_web_session(&session)
                .await
            {
                Ok(url) => {
                    let history_error = service.record_jumpserver_rdp_session(session).await.err();
                    Ok((url, history_error))
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if this.opening_rdp_profile.as_ref() != Some(&profile_id) {
                    return;
                }
                this.opening_rdp_profile = None;
                match result {
                    Ok((url, None)) => {
                        cx.open_url(&url);
                        this.notice = Some(Notice::info("已在浏览器中打开远程桌面"));
                    }
                    Ok((url, Some(error))) => {
                        cx.open_url(&url);
                        this.notice = Some(Notice::error(format!(
                            "远程桌面已打开，但保存最近会话失败：{}",
                            error.message()
                        )));
                    }
                    Err(error) => {
                        this.notice = Some(Notice::error(format!(
                            "打开远程桌面失败：{}",
                            error.message()
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_profile_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_profile_form(None, window, cx);
    }

    pub(super) fn open_jumpserver_assets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let service = self.service.clone();
        let panel = cx.new(|cx| JumpServerPanel::new(service, window, cx));
        self.jumpserver_subscription =
            Some(cx.subscribe_in(&panel, window, Self::on_jumpserver_event));

        let panel_for_dialog = panel.clone();
        let view_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let panel = panel_for_dialog.clone();
            let view_for_close = view_for_close.clone();
            dialog
                .title("导入连接")
                .on_close(move |_, _, app| {
                    view_for_close.update(app, |this, _| {
                        this.jumpserver_subscription = None;
                    });
                })
                .w(px(1040.0))
                .pt(px(24.0))
                .px(px(24.0))
                .pb(px(14.0))
                .content(move |content, _, _| content.child(panel.clone()))
        });
    }

    pub(super) fn open_profile_edit(
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
            self.notice = Some(Notice::error("SSH 配置已删除，请刷新"));
            cx.notify();
            return;
        };
        self.open_profile_form(Some(profile), window, cx);
    }

    fn open_profile_form(
        &mut self,
        profile: Option<SshProfile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let service = self.service.clone();
        let capability = self.default_capability.clone();
        let form = cx.new(|cx| SshProfileFormPanel::new(service, profile, capability, window, cx));
        self.profile_form_subscription =
            Some(cx.subscribe_in(&form, window, Self::on_profile_form_event));

        let title = form.read(cx).title().to_string();
        let form_for_dialog = form.clone();
        let form_for_cancel = form.clone();
        let view_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_cancel.clone();
            let view_for_close = view_for_close.clone();
            dialog
                .title(title.clone())
                .close_button(false)
                .on_cancel(move |_, window, app| {
                    if form_for_cancel.read(app).is_busy() {
                        return false;
                    }
                    if !form_for_cancel.read(app).is_dirty(app) {
                        return true;
                    }
                    let form_inner = form_for_cancel.clone();
                    ramag_ui::open_confirm(
                        "放弃？",
                        "未保存内容将丢失。",
                        "放弃",
                        true,
                        move |_, app| {
                            form_inner.update(app, |_this, cx| {
                                cx.emit(ProfileFormEvent::Cancelled);
                            });
                        },
                        window,
                        app,
                    );
                    false
                })
                .on_close(move |_, _, app| {
                    view_for_close.update(app, |this, _| {
                        this.profile_form_subscription = None;
                    });
                })
                .w(px(720.0))
                .pt(px(24.0))
                .px(px(24.0))
                .pb(px(14.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    fn on_jumpserver_event(
        &mut self,
        _panel: &Entity<JumpServerPanel>,
        event: &JumpServerEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            JumpServerEvent::Saved(profile) => {
                let profile = profile.as_ref().clone();
                self.upsert_profile(profile.clone());
                if let Some(workspace) = self.workspace_mut(&profile.id) {
                    workspace.profile = profile;
                }
                cx.notify();
            }
        }
    }

    fn on_profile_form_event(
        &mut self,
        _form: &Entity<SshProfileFormPanel>,
        event: &ProfileFormEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ProfileFormEvent::SaveRequested(profile) => {
                let profile = profile.as_ref().clone();
                self.request_profile_save(_form.clone(), profile, window, cx);
            }
            ProfileFormEvent::Cancelled => {
                if let Some(profile_id) = _form.read(cx).editing_id.clone() {
                    self.service.unblock_terminal_launches(&profile_id);
                }
                self.profile_form_subscription = None;
                window.close_dialog(cx);
            }
            ProfileFormEvent::CapabilityChanged(capability) => {
                self.capability_generation = self.capability_generation.wrapping_add(1);
                self.default_capability = Some(capability.clone());
                cx.notify();
            }
        }
    }

    fn request_profile_save(
        &mut self,
        form: Entity<SshProfileFormPanel>,
        profile: SshProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_production = self
            .profiles
            .iter()
            .find(|current| current.id == profile.id)
            .is_some_and(|current| current.production);
        if previous_production && !profile.production {
            let expected = profile.name.clone();
            let entity = cx.entity();
            let form_for_confirm = form.clone();
            let profile_for_confirm = profile.clone();
            ramag_ui::open_bounded_prompt(
                "关闭生产保护？",
                format!(
                    "将允许完整 SSH Terminal。请输入连接名称「{}」确认。",
                    profile.name
                ),
                "",
                "关闭保护",
                ramag_domain::entities::MAX_SSH_PROFILE_NAME_BYTES,
                move |value, window, app| {
                    if value != expected {
                        form_for_confirm.update(app, |form, cx| {
                            form.save_failed("连接名称不匹配", cx);
                        });
                        return;
                    }
                    entity.update(app, |this, cx| {
                        this.persist_profile(
                            form_for_confirm,
                            profile_for_confirm,
                            false,
                            window,
                            cx,
                        );
                    });
                },
                window,
                cx,
            );
            return;
        }

        let has_live_terminal = self
            .workspace_mut(&profile.id)
            .is_some_and(|workspace| !workspace.terminals.is_empty() || workspace.terminal_loading);
        if !previous_production && profile.production && has_live_terminal {
            let terminal_count = self
                .workspace_mut(&profile.id)
                .map_or(0, |workspace| workspace.terminals.len());
            self.service.block_terminal_launches(&profile.id);
            if let Some(workspace) = self.workspace_mut(&profile.id) {
                workspace.terminal_generation = workspace.terminal_generation.wrapping_add(1);
                workspace.terminal_loading = false;
                for terminal in &workspace.terminals {
                    terminal.view.update(cx, |terminal, _| {
                        terminal.core().set_input_enabled(false);
                    });
                }
            }
            let entity = cx.entity();
            let profile_id = profile.id.clone();
            let cancel_entity = entity.clone();
            ramag_ui::open_confirm_with_cancel(
                "开启生产保护？",
                format!("将冻结并关闭 {terminal_count} 个完整终端。已执行的远端操作无法撤销。"),
                "关闭终端并开启",
                true,
                (
                    move |window, app| {
                        entity.update(app, |this, cx| {
                            this.persist_profile(form, profile, true, window, cx);
                        });
                    },
                    move |_, app| {
                        cancel_entity.update(app, |this, cx| {
                            this.service.unblock_terminal_launches(&profile_id);
                            if let Some(workspace) = this.workspace_mut(&profile_id) {
                                for terminal in &workspace.terminals {
                                    terminal.view.update(cx, |terminal, _| {
                                        terminal.core().set_input_enabled(true);
                                    });
                                }
                            }
                            cx.notify();
                        });
                    },
                ),
                window,
                cx,
            );
            return;
        }

        let close_terminals = !previous_production && profile.production;
        self.persist_profile(form, profile, close_terminals, window, cx);
    }

    fn persist_profile(
        &mut self,
        form: Entity<SshProfileFormPanel>,
        profile: SshProfile,
        close_terminals: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        form.update(cx, |form, cx| form.begin_save(cx));
        if close_terminals {
            self.service.block_terminal_launches(&profile.id);
            if let Some(workspace) = self.workspace_mut(&profile.id) {
                workspace.terminal_generation = workspace.terminal_generation.wrapping_add(1);
                workspace.terminal_loading = false;
                for terminal in &workspace.terminals {
                    terminal.view.update(cx, |terminal, _| {
                        terminal.core().set_input_enabled(false);
                        terminal.core_mut().close();
                    });
                }
            }
        }

        let service = self.service.clone();
        let profile_id = profile.id.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            if close_terminals {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let completed = this
                        .update_in(async_cx, |this, _window, cx| {
                            this.workspace_mut(&profile_id).is_none_or(|workspace| {
                                workspace.terminals.iter().all(|terminal| {
                                    terminal.view.read(cx).core().shutdown_complete()
                                })
                            })
                        })
                        .unwrap_or(false);
                    if completed {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let _ = form.update_in(async_cx, |form, _, cx| {
                            form.save_failed("完整终端未能在 5 秒内关闭，请重试", cx);
                        });
                        return;
                    }
                    async_cx
                        .background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                }
            }

            let result = service.save_profile(&profile).await;
            let _ = this.update_in(async_cx, |this, window, cx| match result {
                Ok(()) => {
                    service.unblock_terminal_launches(&profile.id);
                    this.profile_form_subscription = None;
                    window.close_dialog(cx);
                    this.upsert_profile(profile.clone());
                    let profile_id = profile.id.clone();
                    let production = profile.production;
                    if let Some(workspace) = this.workspace_mut(&profile_id) {
                        if close_terminals {
                            workspace.terminals.clear();
                            workspace.active_terminal_id = None;
                        }
                        workspace.profile = profile;
                    }
                    if production {
                        this.probe_workspace_capabilities(profile_id, cx);
                    }
                    this.notice = Some(Notice::info("已保存"));
                    cx.notify();
                }
                Err(error) => {
                    service.unblock_terminal_launches(&profile.id);
                    form.update(cx, |form, cx| form.save_failed(error.to_string(), cx));
                }
            });
        })
        .detach();
    }

    fn upsert_profile(&mut self, profile: SshProfile) {
        let mut profiles = self.profiles.as_ref().clone();
        if let Some(existing) = profiles
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile;
        } else {
            profiles.push(profile);
        }
        profiles.sort_by(|left, right| left.name.cmp(&right.name));
        self.profiles = Arc::new(profiles);
    }

    pub(super) fn request_delete_profile(
        &mut self,
        id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.deleting_profile {
            return;
        }
        let Some(profile) = self.profiles.iter().find(|profile| profile.id == id) else {
            return;
        };
        let entity = cx.entity();
        let name = profile.name.clone();
        ramag_ui::open_confirm(
            "删除？",
            format!("将永久删除「{name}」及关联工作区、传输。"),
            "删除",
            true,
            move |window, app| {
                entity.update(app, |this, cx| this.delete_profile(id, window, cx));
            },
            window,
            cx,
        );
    }

    fn delete_profile(&mut self, id: SshProfileId, window: &mut Window, cx: &mut Context<Self>) {
        if self.deleting_profile {
            return;
        }
        self.deleting_profile = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let result = service.delete_profile(&id).await;
            let profiles = if result.is_ok() {
                service.list_profiles().await
            } else {
                Ok(Vec::new())
            };
            let _ = this.update(async_cx, |this, cx| {
                this.deleting_profile = false;
                match (result, profiles) {
                    (Ok(()), Ok(profiles)) => {
                        this.profiles = Arc::new(profiles);
                        this.workspaces
                            .retain(|workspace| workspace.profile_id() != &id);
                        this.path_favorites.remove(&id);
                        if this.active_workspace_id.as_ref() == Some(&id) {
                            this.active_workspace_id = this
                                .workspaces
                                .first()
                                .map(|workspace| workspace.profile.id.clone());
                        }
                        this.notice = Some(Notice::info("已删除"));
                        this.persist_workspaces(cx);
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        this.notice = Some(Notice::error(format!("删除失败：{error}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
