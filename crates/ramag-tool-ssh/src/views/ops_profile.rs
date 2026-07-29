//! SSH 连接列表、配置弹窗与删除操作。

use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, ParentElement, Styled, Window, px};
use gpui_component::WindowExt as _;
use ramag_domain::entities::{SshProfile, SshProfileId};

use super::SshView;
use super::model::Notice;
use super::profile_dialog::{ProfileFormEvent, SshProfileFormPanel};

impl SshView {
    pub(super) fn open_profile_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_profile_form(None, window, cx);
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

    fn on_profile_form_event(
        &mut self,
        _form: &Entity<SshProfileFormPanel>,
        event: &ProfileFormEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ProfileFormEvent::Saved(profile) => {
                let profile = profile.as_ref().clone();
                self.profile_form_subscription = None;
                window.close_dialog(cx);
                self.upsert_profile(profile.clone());
                if let Some(workspace) = self.workspace_mut(&profile.id) {
                    workspace.profile = profile;
                }
                self.notice = Some(Notice::info("已保存"));
                cx.notify();
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
