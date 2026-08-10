mod card;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, ParentElement, Render,
    ScrollHandle, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    v_flex,
};
use ramag_app::ClipboardService;
use ramag_domain::entities::ClipItem;

use crate::views::helpers::filter_items;

/// 可见条目上限。
const DRAWER_LIMIT: usize = 60;

const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
/// 后台搜索多取一段，供缓存结果去重。
const DRAWER_SEARCH_LIMIT: usize = 200;

pub struct ClipboardDrawer {
    service: Arc<ClipboardService>,
    items: Vec<Arc<ClipItem>>,
    search_results: Vec<Arc<ClipItem>>,
    search_truncated: bool,
    /// 用于丢弃过期搜索结果。
    search_gen: u64,
    /// 新输入或关闭抽屉时终止旧搜索。
    search_cancel: Arc<AtomicBool>,
    selected: usize,
    search: Entity<InputState>,
    /// 自动粘贴前恢复的原窗口。
    activation_target: Option<String>,
    auto_paste: bool,
    pending_notification: Option<Notification>,
    /// 防止异步关窗前重复触发粘贴。
    pasting: bool,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    pub(super) img_cache: crate::views::image_cache::ImageCache,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for ClipboardDrawer {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for ClipboardDrawer {
    fn drop(&mut self) {
        self.search_cancel.store(true, Ordering::Relaxed);
    }
}

impl ClipboardDrawer {
    pub fn new(
        service: Arc<ClipboardService>,
        activation_target: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("搜索…"));

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.selected = 0;
                    this.schedule_search(cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.paste(this.selected, window, cx),
                _ => {}
            },
        ));
        search.update(cx, |s, cx| s.focus(window, cx));

        let items = service.cached_snapshot();
        Self {
            service: service.clone(),
            items,
            search_results: Vec::new(),
            search_truncated: false,
            search_gen: 0,
            search_cancel: Arc::new(AtomicBool::new(false)),
            selected: 0,
            search,
            activation_target,
            auto_paste: service.auto_paste(),
            pending_notification: None,
            pasting: false,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            img_cache: crate::views::image_cache::ImageCache::new(),
            _subscriptions: subs,
        }
    }

    pub(super) fn service(&self) -> &Arc<ClipboardService> {
        &self.service
    }

    /// 缓存未命中时异步加载缩略图。
    pub(super) fn thumb_image(
        &self,
        item: Arc<ClipItem>,
        cx: &mut Context<Self>,
    ) -> Option<std::sync::Arc<gpui::Image>> {
        let path = item
            .thumb_path
            .clone()
            .or_else(|| item.image_path.clone())?;
        if let Some(img) = self.img_cache.peek(&path) {
            return Some(img);
        }
        if self.img_cache.begin_load(&path) {
            let svc = self.service.clone();
            cx.spawn(async move |this, cx| {
                let loaded = svc.load_thumb(item.as_ref()).await;
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

    /// 合并即时缓存与后台搜索结果。
    pub(super) fn visible_items(&self, cx: &gpui::App) -> Vec<Arc<ClipItem>> {
        self.visible_items_with_status(cx).0
    }

    fn visible_items_with_status(&self, cx: &gpui::App) -> (Vec<Arc<ClipItem>>, bool) {
        let search = self.search.read(cx);
        let q = search.value();
        if q.trim().is_empty() {
            let mut items = filter_items(&self.items, "", None)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            let truncated = items.len() > DRAWER_LIMIT;
            items.truncate(DRAWER_LIMIT);
            return (items, truncated);
        }
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<Arc<ClipItem>> = Vec::new();
        for it in filter_items(&self.items, &q, None) {
            seen.insert(it.id.clone());
            out.push(it.clone());
        }
        for it in &self.search_results {
            if !seen.contains(&it.id) {
                out.push(it.clone());
            }
        }
        let truncated = out.len() > DRAWER_LIMIT || self.search_truncated;
        out.truncate(DRAWER_LIMIT);
        (out, truncated)
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        let query = self.search.read(cx).value().to_string();
        self.search_cancel.store(true, Ordering::Relaxed);
        // 去抖期间不能混入上一轮结果。
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
                .search_cancellable(&query, DRAWER_SEARCH_LIMIT, cancelled)
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
                        tracing::warn!(
                            operation = "clipboard_drawer_search",
                            query_bytes = query.len(),
                            error = %e,
                            "drawer full search failed"
                        );
                        this.search_results.clear();
                        this.search_truncated = false;
                        this.pending_notification = Some(Notification::warning(format!(
                            "全量搜索失败（仅显示最近缓存）：{e}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 保持抽屉在前台直至恢复原窗口，满足 Windows 激活限制。
    pub(super) fn paste(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.pasting {
            return;
        }
        let Some(item) = self.visible_items(cx).get(idx).cloned() else {
            return;
        };
        self.pasting = true;
        let svc = self.service.clone();
        let target = self.activation_target.clone();
        let auto = self.auto_paste;
        let mode = if auto { "auto_paste" } else { "copy" };
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = if auto {
                svc.paste_to_app(item.as_ref(), target.as_deref()).await
            } else {
                svc.copy_to_clipboard(item.as_ref()).await
            };
            match result {
                Ok(()) => {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }
                Err(e) => {
                    tracing::warn!(
                        operation = "clipboard_drawer_paste",
                        clip_id = %item.id,
                        mode,
                        error = %e,
                        "drawer paste failed"
                    );
                    let _ = this.update(cx, |this, cx| {
                        this.pasting = false;
                        this.pending_notification = Some(Notification::warning(e.to_string()));
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let n = self.visible_items(cx).len();
        if n == 0 {
            return;
        }
        let cur = self.selected.min(n - 1) as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
        if next != self.selected {
            self.selected = next;
            self.scroll.scroll_to_item(next);
            cx.notify();
        }
    }

    /// 左右键由输入框处理，因此上下键用于切换卡片。
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        if key == "escape" {
            window.remove_window();
            return;
        }
        if !ev.keystroke.modifiers.modified() {
            match key {
                "up" => {
                    self.move_selection(-1, cx);
                    return;
                }
                "down" => {
                    self.move_selection(1, cx);
                    return;
                }
                _ => {}
            }
        }
        if ev.keystroke.modifiers.secondary()
            && ev.keystroke.modifiers.number_of_modifiers() == 1
            && key.len() == 1
            && key.chars().all(|c| ('1'..='9').contains(&c))
        {
            let idx = key.parse::<usize>().unwrap_or(1) - 1;
            self.paste(idx, window, cx);
        }
    }

    fn render_topbar(&self, truncated: bool) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_none()
            .h(px(44.0))
            .items_center()
            .justify_center()
            .px(px(16.0))
            .child(
                div()
                    .w(px(360.0))
                    .max_w_full()
                    .child(Input::new(&self.search).small()),
            )
            .when(truncated, |bar| {
                bar.child(div().text_xs().child("仅显示前 60 条"))
            })
    }
}

impl Render for ClipboardDrawer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }

        // 释放主题借用，避免与 render_card 的可变借用冲突。
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let focus = self.focus_handle.clone();

        let (visible, truncated) = self.visible_items_with_status(cx);
        if self.selected >= visible.len() {
            self.selected = visible.len().saturating_sub(1);
        }
        let empty = visible.is_empty();

        let topbar = self.render_topbar(truncated).into_any_element();
        // 闭包会让 render_card 的可变借用逃逸。
        let mut cards = Vec::with_capacity(visible.len());
        for (ix, item) in visible.iter().enumerate() {
            cards.push(self.render_card(ix, item.clone(), cx).into_any_element());
        }

        v_flex()
            .key_context("ClipboardDrawer")
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.on_key(ev, window, cx);
            }))
            .size_full()
            .bg(bg)
            .border_t_1()
            .border_color(border)
            .child(topbar)
            .child(
                h_flex()
                    .id("drawer-strip")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap(px(12.0))
                    .px(px(16.0))
                    .pb(px(12.0))
                    .overflow_x_scroll()
                    .track_scroll(&self.scroll)
                    .when(empty, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(muted)
                                .child("暂无剪贴历史"),
                        )
                    })
                    .children(cards),
            )
    }
}
