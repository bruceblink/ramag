//! ClipboardView 异步操作：重载 / 复制 / 删除 / 清空 / 键盘导航

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Context, ScrollStrategy};
use gpui_component::{Disableable as _, notification::Notification};
use ramag_domain::entities::{ClipId, ClipItem};
use tracing::error;

use super::ClipboardView;
use crate::views::helpers::filter_items;

/// 图片删除的撤销宽限期；到期仅清理仍未被任何条目引用的媒体。
const DELETE_UNDO_GRACE: Duration = Duration::from_secs(30);

/// 全量搜索去抖：输入停顿此间隔后才触发后台扫描
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
/// 全量搜索结果上限
const SEARCH_LIMIT: usize = 500;

impl ClipboardView {
    /// 同步从 service 缓存重载最近窗口快照（无 IO、无解密；旧版异步全表解密已废弃）
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.loaded_revision = self.service.revision();
        self.items = self.service.cached_snapshot();
        // 选中项若已不在缓存窗口且也不在搜索结果中才清空——否则会误清"仅搜索命中"的选中
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

    /// 当前过滤+排序后的可见条目（仅 clone Arc，正文由缓存共享）。
    pub(super) fn visible_items(&self, cx: &gpui::App) -> Vec<Arc<ClipItem>> {
        let search = self.search.read(cx);
        let query = search.value();
        if query.trim().is_empty() {
            return filter_items(&self.items, "", self.filter)
                .into_iter()
                .cloned()
                .collect();
        }
        // 即时层：缓存窗口匹配（输入即显示）；补充层：后台全量结果（去重，缓存优先）
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

    /// 搜索框变化：去抖后台全量搜索，补充缓存窗口之外的匹配
    pub(super) fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        let query = self.search.read(cx).value().to_string();
        self.search_cancel.store(true, Ordering::Relaxed);
        // 新查询不能沿用上一关键词的后台结果，否则去抖期间会短暂展示错误命中。
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
            // 去抖：期间又有输入则代号已变，放弃本次
            if this
                .update(cx, |this, _| this.search_gen != generation)
                .unwrap_or(true)
            {
                return;
            }
            // 搜索失败必须明示（解密 / 存储错误），不得伪装成「无结果」
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

    /// 浏览器打开链接（同步调用，失败弹通知）
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

    /// 在系统文件管理器中显示文件
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
                                // action 通知默认永不自动消失；统一按实际删除时刻收起，避免通知列表
                                // 与其捕获的大正文长期驻留。延迟打开视图时也不能重新获得完整 30 秒。
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
                                        // 先收起 toast，再异步回存并刷新列表
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

    /// 键盘上/下移动选中（基于可见列表）
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
        // 先查缓存窗口，再查搜索结果——选中的可能是"仅搜索命中"（窗口外）的旧记录，
        // 否则详情空白、回车复制 / 删除静默失效
        self.items
            .iter()
            .find(|i| &i.id == sel)
            .or_else(|| self.search_results.iter().find(|i| &i.id == sel))
            .cloned()
    }

    /// 取图片的解密内存图片（thumb=true 用缩略图，否则原图）。
    /// 缓存命中同步返回；miss 异步解密填充后 notify，本帧返回 None（占位）
    /// 该条目的图片是否已判定解密 / 解码失败（详情页显示失败文案而非永久「加载中」）
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
                    _ => {
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
