use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, ParentElement as _, Styled as _, Window, px};
use gpui_component::{WindowExt as _, notification::Notification};
use ramag_app::{AccountVerification, SavedObjectStorageAccount};
use ramag_domain::entities::{ObjectStorageAccount, ObjectStorageAccountId};

use super::{
    account_form::{AccountFormEvent, AccountFormPanel},
    model::{AccountSessionState, ObjectStorageView},
};

impl ObjectStorageView {
    pub(super) fn load_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let load_session_preference = !self.session_preference_loaded;
        let preserve_management = !load_session_preference && self.management_visible;
        self.session_preference_loaded = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.list_accounts().await;
            let session = if load_session_preference {
                Some(service.load_session_preference().await)
            } else {
                None
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(accounts) => {
                        let saved_active = session
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .and_then(|preference| preference.active_account_id.clone());
                        if let Some(Ok(preference)) = &session {
                            this.open_account_ids = preference
                                .open_account_ids
                                .iter()
                                .filter(|id| accounts.iter().any(|account| &account.id == *id))
                                .cloned()
                                .collect();
                        }
                        let selected = this
                            .selected_account_id
                            .clone()
                            .filter(|id| accounts.iter().any(|account| &account.id == id))
                            .or_else(|| {
                                saved_active.filter(|id| {
                                    this.open_account_ids.contains(id)
                                        && accounts.iter().any(|account| &account.id == id)
                                })
                            })
                            .or_else(|| {
                                this.open_account_ids
                                    .iter()
                                    .find(|id| accounts.iter().any(|account| &account.id == *id))
                                    .cloned()
                            });
                        this.accounts = Arc::new(accounts);
                        this.account_session_states
                            .retain(|id, _| this.accounts.iter().any(|account| &account.id == id));
                        if preserve_management {
                            this.management_visible = true;
                        } else if let Some(id) = selected {
                            this.select_account(id, window, cx);
                        } else {
                            this.selected_account_id = None;
                            this.mounts = Arc::new(Vec::new());
                            this.selected_mount = None;
                            this.capabilities = None;
                            this.management_visible = true;
                            if matches!(&session, Some(Ok(_))) {
                                this.persist_session_preference(cx);
                            }
                        }
                        if let Some(Err(error)) = &session {
                            tracing::error!(operation = "object_storage_session_load", error = %error, "load object storage session failed");
                            this.notice =
                                Some((format!("会话偏好加载失败：{}", error.user_message()), true));
                        }
                    }
                    Err(error) => this.operation_error(
                        "object_storage_account_list",
                        &error,
                        format!("加载账号失败：{}", error.user_message()),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_account(
        &mut self,
        id: ObjectStorageAccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_account_id = Some(id.clone());
        if !self.open_account_ids.contains(&id) {
            self.open_account_ids.push(id.clone());
        }
        self.persist_session_preference(cx);
        self.management_visible = false;
        self.show_detail = false;
        self.selected_mount = None;
        self.capabilities = None;
        Arc::make_mut(&mut self.entries).clear();
        self.listing_request_id = self.listing_request_id.wrapping_add(1);
        self.detail_request_id = self.detail_request_id.wrapping_add(1);
        self.prefix.clear();
        self.workspace_states.clear();
        self.favorites.clear();
        self.set_form_value(&self.object_filter.clone(), "", window, cx);
        self.clear_object_detail("选择挂载点后浏览对象");
        self.explorer_resize = cx.new(|_| gpui_component::resizable::ResizableState::default());
        self.load_mounts(id, window, cx);
    }

    pub(super) fn load_mounts(
        &mut self,
        account_id: ObjectStorageAccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = true;
        self.account_session_states
            .insert(account_id.clone(), AccountSessionState::Loading);
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.list_mounts(&account_id).await;
            let workspace = service.load_workspace(&account_id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.selected_account_id.as_ref() != Some(&account_id) {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(result) => {
                        this.mounts = Arc::new(result.mounts);
                        if let Ok(workspace) = &workspace {
                            this.workspace_states = workspace.workspaces.clone();
                            this.favorites = workspace.favorites.clone();
                            this.show_mounts = workspace.show_mounts;
                            this.show_detail = false;
                        }
                        if this.selected_mount.is_none() {
                            let saved_selection = workspace.as_ref().ok().and_then(|workspace| {
                                let saved = workspace
                                    .workspaces
                                    .iter()
                                    .find(|saved| saved.account_id == account_id)?;
                                let mount_id = saved.mount_id.as_ref()?;
                                let mount = this
                                    .mounts
                                    .iter()
                                    .find(|mount| &mount.id == mount_id)
                                    .cloned()?;
                                Some((mount, String::new()))
                            });
                            let selection = saved_selection.or_else(|| {
                                this.mounts
                                    .first()
                                    .cloned()
                                    .map(|mount| (mount, String::new()))
                            });
                            if let Some((mount, prefix)) = selection {
                                this.selected_mount = Some(mount);
                                this.capabilities = None;
                                this.prefix = prefix;
                                this.load_first_page(window, cx);
                            }
                        }
                        if let Err(error) = &workspace {
                            tracing::error!(operation = "object_storage_workspace_load", account_id = %account_id, error = %error, "load object storage workspace failed");
                            this.notice = Some((
                                format!("工作区偏好加载失败：{}", error.user_message()),
                                true,
                            ));
                        }
                        this.account_session_states
                            .insert(account_id.clone(), AccountSessionState::Configured);
                    }
                    Err(error) => {
                        this.account_session_states
                            .insert(account_id.clone(), AccountSessionState::Unverified);
                        this.operation_error(
                            "object_storage_mount_list",
                            &error,
                            format!("加载挂载点失败：{}", error.user_message()),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn show_new_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_account_form(None, window, cx);
    }

    pub(super) fn show_edit_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self
            .selected_account_id
            .as_ref()
            .and_then(|id| self.accounts.iter().find(|account| &account.id == id))
            .cloned()
        else {
            self.error("账号已不存在，请刷新后重试");
            cx.notify();
            return;
        };
        self.open_account_form(Some(account), window, cx);
    }

    fn open_account_form(
        &mut self,
        account: Option<ObjectStorageAccount>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let service = self.service.clone();
        let form = cx.new(|cx| AccountFormPanel::new(service, account, window, cx));
        self.account_form_subscription =
            Some(cx.subscribe_in(&form, window, Self::on_account_form_event));
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
                    if form_for_cancel.read(app).is_saving() {
                        return false;
                    }
                    if !form_for_cancel.read(app).is_dirty(app) {
                        return true;
                    }
                    let form = form_for_cancel.clone();
                    ramag_ui::open_confirm(
                        "放弃修改？",
                        "表单有未保存的修改，关闭将丢弃这些修改。",
                        "放弃修改",
                        true,
                        move |_, app| {
                            form.update(app, |_this, cx| {
                                cx.emit(AccountFormEvent::Cancelled);
                            });
                        },
                        window,
                        app,
                    );
                    false
                })
                .on_close(move |_, _, app| {
                    view_for_close.update(app, |this, _| {
                        this.account_form_subscription = None;
                    });
                })
                .w(px(720.0))
                .pt(px(24.0))
                .px(px(24.0))
                .pb(px(14.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    fn on_account_form_event(
        &mut self,
        _form: &Entity<AccountFormPanel>,
        event: &AccountFormEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AccountFormEvent::Saved(saved) => {
                self.account_form_subscription = None;
                window.close_dialog(cx);
                let (message, warning) = saved_account_feedback(saved);
                let notification = if warning {
                    Notification::warning(message)
                } else {
                    Notification::success(message)
                };
                window.push_notification(notification, cx);
                self.load_accounts(window, cx);
            }
            AccountFormEvent::Cancelled => {
                self.account_form_subscription = None;
                window.close_dialog(cx);
            }
        }
    }

    pub(super) fn request_delete_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_account_id.clone() else {
            return;
        };
        let name = self
            .accounts
            .iter()
            .find(|account| account.id == id)
            .map(|account| account.name.clone())
            .unwrap_or_else(|| "未知账号".into());
        let closes_session = self.open_account_ids.contains(&id);
        let transfer_count = self
            .transfers
            .iter()
            .filter(|transfer| transfer.account_id == id)
            .count();
        let view = cx.entity();
        ramag_ui::open_confirm(
            "删除云存储账号？",
            format!(
                "确定删除「{name}」吗？账号配置和工作区偏好将从本机删除；关闭会话：{}；取消传输：{transfer_count} 个。远端 Bucket 和对象不会被删除。",
                if closes_session { "是" } else { "否" }
            ),
            "删除",
            true,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.confirm_delete_account(id.clone(), window, cx);
                });
            },
            window,
            cx,
        );
    }

    pub(super) fn show_account_management(&mut self, cx: &mut Context<Self>) {
        self.management_visible = true;
        cx.notify();
    }

    pub(super) fn close_session(
        &mut self,
        id: ObjectStorageAccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_account_ids.retain(|open_id| open_id != &id);
        self.account_session_states.remove(&id);
        for transfer in self
            .transfers
            .iter()
            .filter(|transfer| transfer.account_id == id)
        {
            transfer.cancellation.cancel();
        }
        self.transfers.retain(|transfer| transfer.account_id != id);
        let was_selected = self.selected_account_id.as_ref() == Some(&id);
        if was_selected {
            self.selected_account_id = None;
            self.selected_mount = None;
            self.capabilities = None;
            Arc::make_mut(&mut self.entries).clear();
        }
        self.persist_session_preference(cx);
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.close_account_session(&id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) if was_selected => {
                        if let Some(next) = this.open_account_ids.last().cloned() {
                            this.select_account(next, window, cx);
                        } else {
                            this.management_visible = true;
                        }
                    }
                    Ok(()) => {}
                    Err(error) => this.operation_error(
                        "object_storage_session_close",
                        &error,
                        format!("关闭会话失败：{}", error.user_message()),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_delete_account(
        &mut self,
        id: ObjectStorageAccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.delete_account(&id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(()) => {
                        this.selected_account_id = None;
                        this.selected_mount = None;
                        this.capabilities = None;
                        Arc::make_mut(&mut this.entries).clear();
                        this.open_account_ids.retain(|open_id| open_id != &id);
                        this.account_session_states.remove(&id);
                        this.persist_session_preference(cx);
                        this.management_visible = true;
                        this.notice = Some(("账号及其工作区偏好已删除".into(), false));
                        this.load_accounts(window, cx);
                    }
                    Err(error) => this.operation_error(
                        "object_storage_account_delete",
                        &error,
                        format!("删除账号失败：{}", error.user_message()),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn saved_account_feedback(saved: &SavedObjectStorageAccount) -> (String, bool) {
    match &saved.verification {
        AccountVerification::Verified => (
            format!(
                "账号已保存并验证，已配置 {} 个 Bucket",
                saved.account.manual_buckets.len()
            ),
            false,
        ),
        AccountVerification::Unverified { reason } => {
            (format!("账号已保存，但暂未验证：{reason}"), true)
        }
    }
}

#[cfg(test)]
mod tests {
    use ramag_app::{AccountVerification, SavedObjectStorageAccount};
    use ramag_domain::entities::{CloudProvider, ManualBucket, ObjectStorageAccount};

    use super::saved_account_feedback;

    #[test]
    fn verified_account_reports_configured_bucket_count() {
        let mut account = ObjectStorageAccount::new("logs", CloudProvider::TencentCos);
        account.manual_buckets = vec![ManualBucket::new("logs-bucket", "ap-shanghai")];
        let saved = SavedObjectStorageAccount {
            account,
            verification: AccountVerification::Verified,
        };

        let (message, warning) = saved_account_feedback(&saved);

        assert!(!warning);
        assert_eq!(message, "账号已保存并验证，已配置 1 个 Bucket");
    }
}
