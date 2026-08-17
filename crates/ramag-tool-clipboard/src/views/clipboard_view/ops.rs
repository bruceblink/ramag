//! 剪贴板视图操作。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Context, ScrollStrategy};
use gpui_component::{Disableable as _, notification::Notification};
use ramag_domain::entities::{ClipId, ClipItem};
use tracing::{error, warn};

use super::ClipboardView;
use crate::views::helpers::filter_items;

const DELETE_UNDO_GRACE: Duration = Duration::from_secs(30);

const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
const SEARCH_LIMIT: usize = 500;

impl ClipboardView {
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.loaded_revision = self.service.revision();
        self.items = self.service.cached_snapshot();
        // 保留仅命中全量搜索的选中项。
        if let Some(sel) = &self.selected
            && !self.items.iter().any(|i| &i.id == sel)
            && !self.search_results.iter().any(|i| &i.id == sel)
        {
            self.selected = None;
        }
        cx.notify();
    }

    pub(super) fn load_settings(&mut self, cx: &mut Context<Self>) {
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            svc.load_settings().await;
            let (settings, revision) = svc.settings_snapshot_with_revision();
            let _ = this.update(cx, |this, cx| {
                this.settings = settings;
                this.loaded_settings_revision = revision;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn visible_items(&self, cx: &gpui::App) -> Vec<Arc<ClipItem>> {
        let search = self.search.read(cx);
        let query = search.value();
        if query.trim().is_empty() {
            return filter_items(&self.items, "", self.filter)
                .into_iter()
                .cloned()
                .collect();
        }
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<Arc<ClipItem>> = Vec::new();
        for it in filter_items(&self.items, &query, self.filter) {
            seen.insert(it.id.clone());
            out.push(it.clone());
        }
        for it in &self.search_results {
            if self.filter.is_none_or(|k| it.kind == k) && !seen.contains(&it.id) {
                out.push(it.clone());
            }
        }
        out
    }

    pub(super) fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        let query = self.search.read(cx).value().to_string();
        self.search_cancel.store(true, Ordering::Relaxed);
        self.search_results.clear();
        self.search_truncated = false;
        if query.trim().is_empty() {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.search_cancel = cancelled.clone();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            if this
                .update(cx, |this, _| this.search_gen != generation)
                .unwrap_or(true)
            {
                return;
            }
            let result = svc
                .search_cancellable(&query, SEARCH_LIMIT, cancelled)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.search_gen != generation {
                    return;
                }
                match result {
                    Ok(result) => {
                        this.search_truncated = result.truncated;
                        this.search_results = result.items.into_iter().map(Arc::new).collect();
                    }
                    Err(e) => {
                        error!(
                            operation = "clipboard_search",
                            mode = "full",
                            query_bytes = query.len(),
                            error = %e,
                            "clipboard search failed"
                        );
                        this.search_results.clear();
                        this.search_truncated = false;
                        this.pending_notification = Some(Notification::error(format!(
                            "全量搜索失败（结果仅含最近缓存）：{e}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn copy_clip(&mut self, item: Arc<ClipItem>, cx: &mut Context<Self>) {
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.copy_to_clipboard(item.as_ref()).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.pending_notification = Some(Notification::info("已复制到剪贴板"))
                    }
                    Err(e) => {
                        error!(
                            operation = "clipboard_copy",
                            clip_id = %item.id,
                            error = %e,
                            "copy clipboard entry failed"
                        );
                        this.pending_notification = Some(Notification::error(e.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn copy_plain(&mut self, item: Arc<ClipItem>, cx: &mut Context<Self>) {
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.copy_as_plain_text(item.as_ref()).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.pending_notification = Some(Notification::info("已复制为纯文本"))
                    }
                    Err(e) => {
                        error!(
                            operation = "clipboard_copy_plain",
                            clip_id = %item.id,
                            error = %e,
                            "plain-text copy failed"
                        );
                        this.pending_notification = Some(Notification::error(e.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn open_link(&mut self, url: String, cx: &mut Context<Self>) {
        if let Err(e) = self.service.open_url(&url) {
            error!(
                operation = "clipboard_open_url",
                url_bytes = url.len(),
                error = %e,
                "open URL failed"
            );
            self.pending_notification = Some(Notification::error(e.to_string()));
            cx.notify();
        }
    }

    pub(super) fn reveal_files(&mut self, paths: &[String], cx: &mut Context<Self>) {
        if let Err(e) = self.service.reveal_in_file_manager(paths) {
            error!(
                operation = "clipboard_reveal_files",
                path_count = paths.len(),
                error = %e,
                "reveal in file manager failed"
            );
            self.pending_notification = Some(Notification::error(e.to_string()));
            cx.notify();
        }
    }

    pub(super) fn delete_clip(&mut self, item: Arc<ClipItem>, cx: &mut Context<Self>) {
        let svc = self.service.clone();
        let item_id = item.id.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.delete(item.as_ref()).await;
            let cleanup_token = result.as_ref().ok().copied().flatten();
            let _ = this.update(cx, |this, cx| {
                match result {
                    Err(e) => {
                        error!(
                            operation = "clipboard_delete",
                            clip_id = %item_id,
                            error = %e,
                            "delete clipboard entry failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(format!("删除失败：{e}")));
                    }
                    Ok(_) => {
                        let undo_deadline = Instant::now() + DELETE_UNDO_GRACE;
                        let svc_for_undo = this.service.clone();
                        let view = cx.entity().clone();
                        let item_for_undo = item.clone();
                        let expiry_scheduled = Arc::new(AtomicBool::new(false));
                        this.pending_notification = Some(
                            Notification::info("已删除该条目").action(move |_, window, cx| {
                                // 按原删除时间收起通知，避免长期持有正文。
                                let remaining = undo_remaining(undo_deadline, Instant::now());
                                if !expiry_scheduled.swap(true, Ordering::Relaxed) {
                                    let notification = cx.entity();
                                    let delay = remaining.unwrap_or(Duration::ZERO);
                                    cx.spawn_in(window, async move |_, cx| {
                                        cx.background_executor().timer(delay).await;
                                        let _ = notification.update_in(cx, |note, window, cx| {
                                            note.dismiss(window, cx);
                                        });
                                    })
                                    .detach();
                                }
                                if remaining.is_none() {
                                    return ramag_ui::clickable_button("clip-undo-delete-expired")
                                        .label("已过期")
                                        .disabled(true);
                                }
                                let svc = svc_for_undo.clone();
                                let view = view.clone();
                                let item = item_for_undo.clone();
                                let notif = cx.entity().clone();
                                ramag_ui::clickable_button("clip-undo-delete")
                                    .label("撤销")
                                    .on_click(move |_, window, app| {
                                        let svc = svc.clone();
                                        let view = view.clone();
                                        let item = item.clone();
                                        // 先关闭通知，再异步恢复条目。
                                        notif.update(app, |n, cx| n.dismiss(window, cx));
                                        app.spawn(async move |cx| {
                                            let r = svc.restore(item.as_ref().clone()).await;
                                            view.update(cx, |this, cx| {
                                                if let Err(e) = r {
                                                    error!(
                                                        operation = "clipboard_restore",
                                                        clip_id = %item.id,
                                                        error = %e,
                                                        "restore clipboard entry failed"
                                                    );
                                                    this.pending_notification =
                                                        Some(Notification::error(format!(
                                                            "撤销失败：{e}"
                                                        )));
                                                }
                                                this.reload(cx);
                                            });
                                        })
                                        .detach();
                                    })
                            }),
                        );
                    }
                }
                this.reload(cx);
            });
            if let Some(token) = cleanup_token {
                cx.background_executor().timer(DELETE_UNDO_GRACE).await;
                if let Err(e) = svc.finalize_deleted_media(&item_id, token).await {
                    error!(
                        operation = "clipboard_media_cleanup",
                        clip_id = %item_id,
                        error = %e,
                        "cleanup deleted clipboard media failed"
                    );
                }
            }
        })
        .detach();
    }

    pub(super) fn select_id(&mut self, id: ClipId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        cx.notify();
    }

    pub(super) fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let visible = self.visible_items(cx);
        if visible.is_empty() {
            return;
        }
        let cur = self
            .selected
            .as_ref()
            .and_then(|sel| visible.iter().position(|i| &i.id == sel));
        let next = match cur {
            Some(idx) => (idx as i32 + delta).clamp(0, visible.len() as i32 - 1) as usize,
            None => {
                if delta > 0 {
                    0
                } else {
                    visible.len() - 1
                }
            }
        };
        self.selected = Some(visible[next].id.clone());
        self.list_scroll.scroll_to_item(next, ScrollStrategy::Top);
        cx.notify();
    }

    pub(super) fn selected_item(&self, _cx: &gpui::App) -> Option<Arc<ClipItem>> {
        let sel = self.selected.as_ref()?;
        // 选中项可能只存在于全量搜索结果中。
        self.items
            .iter()
            .find(|i| &i.id == sel)
            .or_else(|| self.search_results.iter().find(|i| &i.id == sel))
            .cloned()
    }

    pub(super) fn image_failed(&self, item: &ClipItem, thumb: bool) -> bool {
        let path = if thumb {
            item.thumb_path.clone().or_else(|| item.image_path.clone())
        } else {
            item.image_path.clone()
        };
        path.is_some_and(|p| self.img_cache.is_failed(&p))
    }

    pub(super) fn image_for(
        &self,
        item: Arc<ClipItem>,
        thumb: bool,
        cx: &mut Context<Self>,
    ) -> Option<std::sync::Arc<gpui::Image>> {
        let path = if thumb {
            item.thumb_path
                .clone()
                .or_else(|| item.image_path.clone())?
        } else {
            item.image_path.clone()?
        };
        if let Some(img) = self.img_cache.peek(&path) {
            return Some(img);
        }
        if self.img_cache.begin_load(&path) {
            let svc = self.service.clone();
            cx.spawn(async move |this, cx| {
                let loaded = if thumb {
                    svc.load_thumb(item.as_ref()).await
                } else {
                    svc.load_image(item.as_ref()).await
                };
                let _ = this.update(cx, |this, cx| match loaded {
                    Ok(Some(bytes)) => {
                        let Some(retained_bytes) =
                            crate::views::image_cache::png_retained_bytes(&bytes)
                        else {
                            warn!(
                                operation = "clipboard_image_load",
                                clip_id = %item.id,
                                asset = if thumb { "thumbnail" } else { "image" },
                                bytes = bytes.len(),
                                "clipboard image data is not a usable PNG"
                            );
                            this.img_cache.fail(&path);
                            cx.notify();
                            return;
                        };
                        let image = std::sync::Arc::new(gpui::Image::from_bytes(
                            gpui::ImageFormat::Png,
                            bytes,
                        ));
                        this.img_cache.insert(path, image, retained_bytes);
                        cx.notify();
                    }
                    Ok(None) => {
                        warn!(
                            operation = "clipboard_image_load",
                            clip_id = %item.id,
                            asset = if thumb { "thumbnail" } else { "image" },
                            "clipboard image data is unavailable"
                        );
                        this.img_cache.fail(&path);
                        cx.notify();
                    }
                    Err(error) => {
                        error!(
                            operation = "clipboard_image_load",
                            clip_id = %item.id,
                            asset = if thumb { "thumbnail" } else { "image" },
                            error = %error,
                            "load clipboard image failed"
                        );
                        this.img_cache.fail(&path);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        None
    }
}

fn undo_remaining(deadline: Instant, now: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(now)
        .filter(|remaining| !remaining.is_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_undo_uses_original_deadline() {
        let started = Instant::now();
        let deadline = started + DELETE_UNDO_GRACE;

        assert_eq!(
            undo_remaining(deadline, started + Duration::from_secs(10)),
            Some(Duration::from_secs(20))
        );
        assert!(undo_remaining(deadline, deadline).is_none());
        assert!(undo_remaining(deadline, deadline + Duration::from_secs(1)).is_none());
    }
}
