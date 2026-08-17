use gpui::{Context, Window};
use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES, ObjectStorageAccountId, ObjectStorageFavorite,
    ObjectStorageMount, ObjectStorageSessionPreference, ObjectStorageWorkspacePreference,
    ObjectStorageWorkspaceState,
};

use super::model::ObjectStorageView;

impl ObjectStorageView {
    pub(super) fn persist_session_preference(&self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        let preference = ObjectStorageSessionPreference {
            open_account_ids: self.open_account_ids.clone(),
            active_account_id: self
                .selected_account_id
                .clone()
                .filter(|id| self.open_account_ids.contains(id)),
        };
        let active_account_id = preference.active_account_id.clone();
        let open_account_count = preference.open_account_ids.len();
        cx.spawn(async move |_this, _cx| {
            if let Err(error) = service.save_session_preference(&preference).await {
                tracing::warn!(
                    operation = "object_storage_session_save",
                    active_account_id = ?active_account_id,
                    open_account_count,
                    error = %error,
                    "save object storage session failed"
                );
            }
        })
        .detach();
    }

    pub(super) fn set_form_value(
        &self,
        input: &gpui::Entity<gpui_component::input::InputState>,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |input, cx| input.set_value(value, window, cx));
    }

    pub(super) fn error(&mut self, message: impl Into<String>) {
        self.notice = Some((message.into(), true));
    }

    pub(super) fn log_operation_failure(
        operation: &'static str,
        account_id: Option<&ObjectStorageAccountId>,
        mount: Option<&ObjectStorageMount>,
        prefix: &str,
        key: Option<&str>,
        error: &(impl std::fmt::Display + ?Sized),
    ) {
        tracing::error!(
            operation,
            account_id = ?account_id,
            mount_id = ?mount.map(|mount| &mount.id),
            bucket = ?mount.map(|mount| mount.bucket.as_str()),
            prefix = %prefix,
            key = ?key,
            error = %error,
            "object storage operation failed"
        );
    }

    pub(super) fn operation_notice(&mut self, message: impl Into<String>) {
        self.error(message);
    }

    pub(super) fn add_path_favorite(
        &mut self,
        mount_id: ramag_domain::entities::ObjectStorageMountId,
        prefix: String,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        let Some(account_id) = self.selected_account_id.clone() else {
            return Err("当前账号已关闭".into());
        };
        if !self.mounts.iter().any(|mount| mount.id == mount_id) {
            return Err("当前挂载点已不存在".into());
        }
        if self
            .favorites
            .iter()
            .any(|favorite| favorite.mount_id == mount_id && favorite.prefix == prefix)
        {
            return Ok(false);
        }
        if self
            .favorites
            .len()
            .saturating_add(self.workspace_states.len())
            >= MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES
        {
            return Err("收藏条目已达到安全上限".into());
        }
        self.favorites.push(ObjectStorageFavorite {
            account_id,
            mount_id,
            prefix,
        });
        self.persist_workspace(cx);
        Ok(true)
    }

    pub(super) fn remove_path_favorite(
        &mut self,
        mount_id: &ramag_domain::entities::ObjectStorageMountId,
        prefix: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self.favorites.len();
        self.favorites.retain(|favorite| {
            &favorite.mount_id != mount_id || favorite.prefix.as_str() != prefix
        });
        let removed = self.favorites.len() != before;
        if removed {
            self.persist_workspace(cx);
        }
        removed
    }

    pub(super) fn persist_workspace(&mut self, cx: &mut Context<Self>) {
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        let mount_id = mount.id.clone();
        let bucket = mount.bucket.clone();
        if let Some(index) = self
            .workspace_states
            .iter()
            .position(|saved| saved.mount_id.as_ref() == Some(&mount.id))
        {
            self.workspace_states.remove(index);
        }
        self.workspace_states.insert(
            0,
            ObjectStorageWorkspaceState {
                account_id: account_id.clone(),
                mount_id: Some(mount_id.clone()),
                // 浏览路径不跨会话恢复。
                prefix: String::new(),
            },
        );
        let service = self.service.clone();
        let preference = ObjectStorageWorkspacePreference {
            active_account_id: Some(account_id.clone()),
            workspaces: self.workspace_states.clone(),
            favorites: self.favorites.clone(),
            show_mounts: self.show_mounts,
            show_detail: false,
        };
        cx.spawn(async move |_this, _cx| {
            if let Err(error) = service.save_workspace(&account_id, &preference).await {
                tracing::warn!(
                    operation = "object_storage_workspace_save",
                    account_id = %account_id,
                    mount_id = %mount_id,
                    bucket = %bucket,
                    error = %error,
                    "save object storage workspace failed"
                );
            }
        })
        .detach();
    }
}
