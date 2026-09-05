mod card;

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, ParentElement, Render,
    ScrollStrategy, Styled, Subscription, Window, div, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme, Sizable as _, VirtualListScrollHandle, h_flex, h_virtual_list,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    v_flex,
};
use ramag_app::ClipboardService;
use ramag_domain::entities::ClipItem;
use tracing::{error, warn};

use crate::views::helpers::filter_items;

/// 可见条目上限。
const DRAWER_LIMIT: usize = 300;
/// 卡片宽度和间距决定虚拟列表列宽。
const CARD_WIDTH: f32 = 232.0;
const CARD_GAP: f32 = 12.0;

const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
/// 后台搜索结果上限。
const DRAWER_SEARCH_LIMIT: usize = DRAWER_LIMIT;

pub struct ClipboardDrawer {
    service: Arc<ClipboardService>,
    items: Vec<Arc<ClipItem>>,
    search_results: Vec<Arc<ClipItem>>,
    search_truncated: bool,
    /// 用于丢弃过期结果。
    search_gen: u64,
    /// 新输入或关闭时终止旧搜索。
    search_cancel: Arc<AtomicBool>,
    selected: usize,
    search: Entity<InputState>,
    /// 自动粘贴的目标窗口。
    activation_target: Option<String>,
    auto_paste: bool,
    pending_notification: Option<Notification>,
    /// 防止关窗前重复粘贴。
    pasting: bool,
    scroll: VirtualListScrollHandle,
    focus_handle: FocusHandle,
    pub(super) img_cache: crate::views::ClipboardImageCache,
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
        self.img_cache.clear_in_flight();
    }
}

impl ClipboardDrawer {
    pub fn new(
        service: Arc<ClipboardService>,
        activation_target: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_image_cache(
            service,
            activation_target,
            crate::views::ClipboardImageCache::new(),
            window,
            cx,
        )
    }

    pub fn with_image_cache(
        service: Arc<ClipboardService>,
        activation_target: Option<String>,
        img_cache: crate::views::ClipboardImageCache,
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
            scroll: VirtualListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            img_cache,
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
                            warn!(
                                operation = "clipboard_drawer_thumbnail_load",
                                clip_id = %item.id,
                                bytes = bytes.len(),
                                "clipboard thumbnail is not a usable PNG"
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
                            operation = "clipboard_drawer_thumbnail_load",
                            clip_id = %item.id,
                            "clipboard thumbnail is unavailable"
                        );
                        this.img_cache.fail(&path);
                        cx.notify();
                    }
                    Err(error) => {
                        error!(
                            operation = "clipboard_drawer_thumbnail_load",
                            clip_id = %item.id,
                            error = %error,
                            "load clipboard thumbnail failed"
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

    /// 合并缓存和后台搜索结果。
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
        // 清除上一轮搜索结果。
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
                        warn!(
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

    /// 保持抽屉前台直至恢复目标窗口。
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
                    warn!(
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
            self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// 上下键切换卡片，左右键保留给输入框。
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

    /// 渲染悬浮抽屉的搜索栏；搜索框和截断提示在窄窗口中分行且保持可见。
    fn render_topbar(&self, truncated: bool) -> impl IntoElement {
        render_topbar(&self.search, truncated)
    }
}

/// 为剪贴板悬浮抽屉提供可独立测试的顶部工具栏布局。
fn render_topbar(search: &Entity<InputState>, truncated: bool) -> impl IntoElement {
    ramag_ui::responsive_toolbar()
        .debug_selector(|| "clipboard-drawer-toolbar".into())
        .flex_none()
        .px(px(12.0))
        .py(px(8.0))
        .child(
            div()
                .debug_selector(|| "clipboard-drawer-search".into())
                .flex_1()
                .min_w(px(128.0))
                .max_w(px(360.0))
                .child(Input::new(search).small()),
        )
        .when(truncated, |bar| {
            bar.child(
                div()
                    .debug_selector(|| "clipboard-drawer-truncated".into())
                    .flex_1()
                    .min_w(px(128.0))
                    .text_xs()
                    .whitespace_normal()
                    .child(format!("仅显示前 {DRAWER_LIMIT} 条")),
            )
        })
}

impl Render for ClipboardDrawer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            ramag_ui::push_responsive_notification(window, n, cx);
        }

        // 先复制主题颜色，释放主题借用。
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
        // 虚拟列表仅构造视口附近卡片。
        let visible = Rc::new(visible);
        let item_sizes = Rc::new(vec![
            size(px(CARD_WIDTH + CARD_GAP), px(0.0));
            visible.len()
        ]);
        let view = cx.entity().clone();
        let cards = h_virtual_list(
            view,
            "drawer-strip",
            item_sizes,
            move |this, range, _, cx| {
                range
                    .map(|ix| {
                        this.render_card(ix, visible[ix].clone(), cx)
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.scroll)
        .size_full()
        .px(px(16.0))
        .pb(px(12.0));

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
                    .flex_1()
                    .min_h_0()
                    .w_full()
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
                    .when(!empty, |this| this.child(cards)),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement,
        ParentElement as _, Pixels, Render, Styled as _, TestAppContext, VisualTestContext, Window,
        div, px, size,
    };
    use gpui_component::input::InputState;

    use super::render_topbar;

    struct DrawerTopbarHost {
        search: Entity<InputState>,
    }

    impl Render for DrawerTopbarHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("clipboard-drawer-toolbar-host")
                .debug_selector(|| "clipboard-drawer-toolbar-host".into())
                .size_full()
                .child(render_topbar(&self.search, true))
        }
    }

    fn assert_inside(parent: &Bounds<Pixels>, child: &Bounds<Pixels>, label: &str) {
        assert!(
            child.origin.x >= parent.origin.x
                && child.origin.y >= parent.origin.y
                && child.right() <= parent.right()
                && child.bottom() <= parent.bottom(),
            "{label} 越出父容器：parent={parent:?}, child={child:?}"
        );
    }

    /// 截断提示较长时，顶部搜索栏仍应在窄窗口内换行并保持两个子项可见。
    #[gpui::test]
    fn drawer_topbar_wraps_search_and_limit_status_inside_narrow_window(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let search = cx.new(|cx| ramag_ui::bounded_search_input(window, cx));
            DrawerTopbarHost { search }
        });
        let cx: &mut VisualTestContext = cx;
        for width in [180.0, 240.0, 600.0] {
            cx.simulate_resize(size(px(width), px(180.0)));
            cx.run_until_parked();

            let host = cx.debug_bounds("clipboard-drawer-toolbar-host");
            assert!(host.is_some(), "剪贴板抽屉工具栏宿主应渲染");
            let toolbar = cx.debug_bounds("clipboard-drawer-toolbar");
            assert!(toolbar.is_some(), "剪贴板抽屉工具栏应渲染");
            let search = cx.debug_bounds("clipboard-drawer-search");
            assert!(search.is_some(), "剪贴板搜索框应渲染");
            let truncated = cx.debug_bounds("clipboard-drawer-truncated");
            assert!(truncated.is_some(), "剪贴板截断提示应渲染");

            if let (Some(host), Some(toolbar), Some(search), Some(truncated)) =
                (host, toolbar, search, truncated)
            {
                assert_inside(&host, &toolbar, "工具栏");
                assert_inside(&toolbar, &search, "搜索框");
                assert_inside(&toolbar, &truncated, "截断提示");
                assert!(search.size.width > px(0.0));
                assert!(truncated.size.width > px(0.0));
                if width <= 240.0 {
                    assert!(truncated.origin.y > search.origin.y);
                }
            }
        }
    }
}
