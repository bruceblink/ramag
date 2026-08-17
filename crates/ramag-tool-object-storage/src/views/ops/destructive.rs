use gpui::{Context, Window};
use ramag_domain::entities::OverwritePolicy;

use super::super::model::ObjectStorageView;

impl ObjectStorageView {
    pub(in crate::views) fn confirm_overwrite_upload(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(conflict) = self.pending_upload.take() {
            self.run_upload(
                conflict.path,
                conflict.key,
                OverwritePolicy::Overwrite,
                window,
                cx,
            );
        }
    }

    pub(in crate::views) fn request_overwrite_upload(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conflict) = self.pending_upload.as_ref() else {
            return;
        };
        let description = format!(
            "Key：{}\n{}\n\n将覆盖现有对象；目标可能在确认前变化。",
            conflict.key, conflict.existing_summary
        );
        let confirm_view = cx.entity();
        let cancel_view = confirm_view.clone();
        ramag_ui::open_confirm_with_cancel(
            "覆盖上传？",
            description,
            "覆盖",
            true,
            (
                move |window, app| {
                    confirm_view.update(app, |this, cx| {
                        this.confirm_overwrite_upload(window, cx);
                    });
                },
                move |_, app| {
                    cancel_view.update(app, |this, _| this.pending_upload = None);
                },
            ),
            window,
            cx,
        );
    }

    pub(in crate::views) fn confirm_overwrite_download(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(conflict) = self.pending_download.take() {
            self.run_download(
                conflict.path,
                conflict.key,
                OverwritePolicy::Overwrite,
                window,
                cx,
            );
        }
    }

    pub(in crate::views) fn request_overwrite_download(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conflict) = self.pending_download.as_ref() else {
            return;
        };
        let description = format!(
            "本地文件：{}\n{}\n\n将原子替换现有文件。",
            conflict.path.display(),
            conflict.existing_summary
        );
        let confirm_view = cx.entity();
        let cancel_view = confirm_view.clone();
        ramag_ui::open_confirm_with_cancel(
            "覆盖下载？",
            description,
            "覆盖",
            true,
            (
                move |window, app| {
                    confirm_view.update(app, |this, cx| {
                        this.confirm_overwrite_download(window, cx);
                    });
                },
                move |_, app| {
                    cancel_view.update(app, |this, _| this.pending_download = None);
                },
            ),
            window,
            cx,
        );
    }

    pub(in crate::views) fn request_delete_object(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(key), Some(account_id), Some(mount)) = (
            self.selected_key.clone(),
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        let account_name = self
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .map(|account| account.name.clone())
            .unwrap_or_else(|| "未知账号".into());
        let view = cx.entity();
        ramag_ui::open_confirm(
            "删除对象",
            format!(
                "账号：{account_name}\nBucket：{}\nKey：{key}\n\n未启用版本控制时通常不可恢复；启用后可能生成 Delete Marker。",
                mount.bucket
            ),
            "删除",
            true,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.confirm_delete_object(key.clone(), window, cx);
                });
            },
            window,
            cx,
        );
    }

    fn confirm_delete_object(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        let prefix = self.prefix.clone();
        self.loading = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.delete_object(&account_id, &mount, &key).await;
            if let Err(error) = &result {
                Self::log_operation_failure(
                    "object_storage_object_delete",
                    Some(&account_id),
                    Some(&mount),
                    &prefix,
                    Some(&key),
                    error,
                );
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(()) => {
                        this.notice =
                            Some(("删除完成；版本控制可能生成 Delete Marker".into(), false));
                        let still_selected = this.selected_account_id.as_ref() == Some(&account_id)
                            && this
                                .selected_mount
                                .as_ref()
                                .is_some_and(|selected| selected.id == mount.id);
                        if still_selected {
                            this.show_detail = false;
                            this.clear_object_detail("双击文件查看内容；右键可查看详情");
                            this.load_first_page(window, cx);
                        }
                    }
                    Err(error) => {
                        this.operation_notice(format!("删除对象失败：{}", error.user_message()))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
