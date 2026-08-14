use gpui::{AppContext as _, Context, Window};
use gpui_component::input::InputState;
use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES, ObjectEntryKind, ObjectStorageMount, OverwritePolicy,
    format_bytes,
};

use super::{
    model::ObjectStorageView,
    object_helpers::{format_object_preview, object_preview_language},
    object_list_helpers::sort_object_entries,
};

mod destructive;

impl ObjectStorageView {
    pub(super) fn select_mount(
        &mut self,
        mount: ObjectStorageMount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_mount = Some(mount);
        self.capabilities = None;
        self.listing_request_id = self.listing_request_id.wrapping_add(1);
        self.detail_request_id = self.detail_request_id.wrapping_add(1);
        self.prefix.clear();
        self.set_form_value(&self.object_filter.clone(), "", window, cx);
        self.show_detail = false;
        self.clear_object_detail("双击文件查看内容；右键可查看详情");
        self.persist_workspace(cx);
        self.load_first_page(window, cx);
    }

    pub(super) fn open_prefix(
        &mut self,
        prefix: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefix = prefix;
        self.set_form_value(&self.object_filter.clone(), "", window, cx);
        self.show_detail = false;
        self.clear_object_detail("双击文件查看内容；右键可查看详情");
        self.load_first_page(window, cx);
    }

    pub(super) fn prompt_object_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_mount.is_none() {
            return;
        }
        let initial = format!("/{}", self.prefix);
        let Some(mount) = self.selected_mount.as_ref() else {
            return;
        };
        let favorites = self
            .favorites
            .iter()
            .filter(|favorite| favorite.mount_id == mount.id)
            .cloned()
            .collect();
        super::path_dialog::open_object_path_dialog(
            cx.entity(),
            mount.id.clone(),
            initial,
            favorites,
            window,
            cx,
        );
    }

    pub(super) fn clear_object_detail(&mut self, message: impl Into<String>) {
        self.selected_key = None;
        self.detail_message = message.into();
        self.detail_metadata = None;
        self.detail_scroll = gpui::ScrollHandle::new();
    }

    pub(super) fn select_entry(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected_key = Some(key);
        cx.notify();
    }

    pub(super) fn open_entry(
        &mut self,
        key: String,
        kind: ObjectEntryKind,
        operable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kind == ObjectEntryKind::Prefix {
            if operable {
                self.open_prefix(key, window, cx);
            } else {
                self.error("此前缀无法安全表示，仅供查看");
                cx.notify();
            }
            return;
        }
        self.open_object_content(key, operable, window, cx);
    }

    fn open_object_content(
        &mut self,
        key: String,
        operable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !operable {
            self.error("该对象键无法安全读取内容");
            cx.notify();
            return;
        }
        if self.show_detail {
            self.show_detail = false;
            self.persist_workspace(cx);
        }
        self.selected_key = Some(key.clone());
        self.detail_request_id = self.detail_request_id.wrapping_add(1);
        let detail_request_id = self.detail_request_id;
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        self.loading = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let preview = service.preview_text_object(&account_id, &mount, &key).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.detail_request_id != detail_request_id {
                    return;
                }
                this.loading = false;
                match preview {
                    Ok(preview) => {
                        let content = format_object_preview(&key, &preview.content);
                        let language = object_preview_language(&key, &content);
                        let line_count = content.lines().count().max(1);
                        let editor = cx.new(|cx| {
                            InputState::new(window, cx)
                                .code_editor(language)
                                .line_number(true)
                                .soft_wrap(false)
                                .indent_guides(false)
                                .folding(false)
                                .default_value(content)
                        });
                        let summary = if preview.truncated {
                            format!(
                                "{} · 仅显示前 {}",
                                format_bytes(preview.total_bytes),
                                format_bytes(MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES as u64)
                            )
                        } else {
                            format_bytes(preview.total_bytes)
                        };
                        super::preview_dialog::open_object_preview_dialog(
                            key.clone(),
                            summary,
                            editor,
                            line_count,
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        this.operation_error(
                            "object_storage_object_preview",
                            &error,
                            format!("查看内容失败：{}", error.user_message()),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn open_object_detail(
        &mut self,
        key: String,
        operable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_detail = true;
        self.persist_workspace(cx);
        self.clear_object_detail("正在读取对象详情…");
        if !operable {
            self.detail_message = "该对象键无法由 OpenDAL 无损表示，因此已禁用下载和删除。".into();
            cx.notify();
            return;
        }
        self.selected_key = Some(key.clone());
        self.detail_request_id = self.detail_request_id.wrapping_add(1);
        let detail_request_id = self.detail_request_id;
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        self.loading = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let metadata = service.stat_object(&account_id, &mount, &key).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.detail_request_id != detail_request_id {
                    return;
                }
                this.loading = false;
                match metadata {
                    Ok(metadata) => {
                        this.detail_message.clear();
                        this.detail_metadata = Some(metadata);
                    }
                    Err(error) => {
                        this.detail_message = "对象详情加载失败，请重试".into();
                        this.operation_error(
                            "object_storage_object_stat",
                            &error,
                            format!("读取对象失败：{}", error.user_message()),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_first_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        let prefix = self.prefix.clone();
        self.listing_request_id = self.listing_request_id.wrapping_add(1);
        let listing_request_id = self.listing_request_id;
        let load_capabilities = self.capabilities.is_none();
        self.persist_workspace(cx);
        self.loading = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let capabilities = if load_capabilities {
                Some(service.capabilities(&account_id, &mount).await)
            } else {
                None
            };
            let result = service
                .start_listing(&account_id, &mount, &prefix, "")
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.listing_request_id != listing_request_id {
                    return;
                }
                this.loading = false;
                if let Some(capabilities) = capabilities {
                    match capabilities {
                        Ok(capabilities) => {
                            if capabilities.write && !capabilities.atomic_create {
                                this.notice =
                                    Some(("当前服务不支持原子防覆盖，已禁用上传".into(), true));
                            }
                            this.capabilities = Some(capabilities);
                        }
                        Err(error) => {
                            this.capabilities = None;
                            this.operation_error(
                                "object_storage_capabilities",
                                &error,
                                format!("读取存储能力失败：{}", error.user_message()),
                            );
                        }
                    }
                }
                match result {
                    Ok(result) => {
                        let mut entries = result.page.entries;
                        sort_object_entries(&mut entries);
                        this.entries = std::sync::Arc::new(entries);
                        this.next_cursor = result.page.next_cursor;
                        this.listing_generation = Some(result.generation);
                        if result.page.capped {
                            this.notice = Some(("对象列表达到 20,000 条工作区上限".into(), true));
                        }
                    }
                    Err(error) => this.operation_error(
                        "object_storage_object_list",
                        &error,
                        format!("列出对象失败：{}", error.user_message()),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_next_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(account_id), Some(mount), Some(cursor), Some(generation)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
            self.next_cursor.clone(),
            self.listing_generation,
        ) else {
            return;
        };
        let prefix = self.prefix.clone();
        let listing_request_id = self.listing_request_id;
        self.loading = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service
                .continue_listing(&account_id, &mount, &prefix, "", &cursor, generation)
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.listing_request_id != listing_request_id {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(result) => {
                        let entries = std::sync::Arc::make_mut(&mut this.entries);
                        entries.extend(result.page.entries);
                        sort_object_entries(entries.as_mut_slice());
                        this.next_cursor = result.page.next_cursor;
                    }
                    Err(error) => this.operation_error(
                        "object_storage_object_list_next",
                        &error,
                        format!("继续加载失败：{}", error.user_message()),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn choose_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.upload_picker_open {
            return;
        }
        self.upload_picker_open = true;
        let prefix = self.prefix.clone();
        cx.spawn_in(window, async move |this, cx| {
            let selected = rfd::AsyncFileDialog::new().pick_file().await;
            let Some(selected) = selected else {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.upload_picker_open = false;
                    cx.notify();
                });
                return;
            };
            let path = selected.path().to_path_buf();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.upload_picker_open = false;
                    this.error("上传文件名必须是 UTF-8");
                    cx.notify();
                });
                return;
            };
            let key = format!("{prefix}{name}");
            let _ = this.update_in(cx, |this, window, cx| {
                this.upload_picker_open = false;
                this.run_upload(path, key, OverwritePolicy::Refuse, window, cx);
            });
        })
        .detach();
    }

    pub(super) fn choose_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.download_picker_open {
            return;
        }
        let Some(key) = self.selected_key.clone() else {
            return;
        };
        if self.download_active_for_key(&key) {
            self.transfers_visible = true;
            self.show_detail = false;
            cx.notify();
            return;
        }
        self.download_picker_open = true;
        let name = key.rsplit('/').next().unwrap_or("download").to_string();
        cx.spawn_in(window, async move |this, cx| {
            let selected = rfd::AsyncFileDialog::new()
                .set_file_name(&name)
                .save_file()
                .await;
            let Some(selected) = selected else {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.download_picker_open = false;
                    cx.notify();
                });
                return;
            };
            let path = selected.path().to_path_buf();
            let _ = this.update_in(cx, |this, window, cx| {
                this.download_picker_open = false;
                this.run_download(path, key, OverwritePolicy::Refuse, window, cx);
            });
        })
        .detach();
    }
}
