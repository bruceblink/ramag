use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, ParentElement, Styled, Window, px};
use gpui_component::WindowExt as _;
use ramag_domain::entities::{JumpServerRdpSession, SshProfile, SshProfileId};

use super::SshView;
use super::jumpserver_dialog::{JumpServerEvent, JumpServerPanel};
use super::model::Notice;
use super::profile_dialog::{ProfileFormEvent, SshProfileFormPanel};

#[derive(Clone, Copy)]
enum RdpWebSessionAction {
    Open,
    Copy,
}

impl SshView {
    pub(super) fn open_profile_rdp(
        &mut self,
        profile_id: SshProfileId,
        session: JumpServerRdpSession,
        cx: &mut Context<Self>,
    ) {
        self.create_profile_rdp_web_session(profile_id, session, RdpWebSessionAction::Open, cx);
    }

    pub(super) fn copy_profile_rdp_link(
        &mut self,
        profile_id: SshProfileId,
        session: JumpServerRdpSession,
        cx: &mut Context<Self>,
    ) {
        self.create_profile_rdp_web_session(profile_id, session, RdpWebSessionAction::Copy, cx);
    }

    fn create_profile_rdp_web_session(
        &mut self,
        profile_id: SshProfileId,
        session: JumpServerRdpSession,
        action: RdpWebSessionAction,
        cx: &mut Context<Self>,
    ) {
        if self.creating_rdp_web_session_profile.is_some() {
            return;
        }
        self.creating_rdp_web_session_profile = Some(profile_id.clone());
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
                if this.creating_rdp_web_session_profile.as_ref() != Some(&profile_id) {
                    return;
                }
                this.creating_rdp_web_session_profile = None;
                match (action, result) {
                    (RdpWebSessionAction::Open, Ok((url, None))) => {
                        cx.open_url(&url);
                        this.notice = Some(Notice::info("已在浏览器中打开远程桌面"));
                    }
                    (RdpWebSessionAction::Open, Ok((url, Some(error)))) => {
                        cx.open_url(&url);
                        this.notice = Some(Notice::error(format!(
                            "远程桌面已打开，但保存最近会话失败：{}",
                            error.message()
                        )));
                    }
                    (RdpWebSessionAction::Copy, Ok((url, None))) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                        this.notice = Some(Notice::info("一次性链接已复制"));
                    }
                    (RdpWebSessionAction::Copy, Ok((url, Some(error)))) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                        this.notice = Some(Notice::error(format!(
                            "链接已复制，但保存最近会话失败：{}",
                            error.message()
                        )));
                    }
                    (_, Err(error)) => {
                        let message = match action {
                            RdpWebSessionAction::Open => "打开远程桌面失败",
                            RdpWebSessionAction::Copy => "复制一次性链接失败",
                        };
                        this.notice =
                            Some(Notice::error(format!("{message}：{}", error.message())));
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
        self.persist_profile(form, profile, window, cx);
    }

    fn persist_profile(
        &mut self,
        form: Entity<SshProfileFormPanel>,
        profile: SshProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        form.update(cx, |form, cx| form.begin_save(cx));
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let result = service.save_profile(&profile).await;
            let _ = this.update_in(async_cx, |this, window, cx| match result {
                Ok(()) => {
                    this.profile_form_subscription = None;
                    window.close_dialog(cx);
                    this.upsert_profile(profile.clone());
                    let profile_id = profile.id.clone();
                    if let Some(workspace) = this.workspace_mut(&profile_id) {
                        workspace.profile = profile;
                    }
                    if this.workspace_mut(&profile_id).is_some() {
                        this.probe_workspace_capabilities(profile_id, cx);
                    }
                    this.notice = Some(Notice::info("已保存"));
                    cx.notify();
                }
                Err(error) => {
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
            let _ = this.update_in(async_cx, |this, window, cx| {
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
                        if let Some(active_id) = this.active_workspace_id.clone() {
                            this.sync_directory_filter(&active_id, window, cx);
                        } else {
                            this.view_mode = super::model::ViewMode::Manager;
                            this.directory_search.update(cx, |state, cx| {
                                state.set_value("", window, cx);
                            });
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
