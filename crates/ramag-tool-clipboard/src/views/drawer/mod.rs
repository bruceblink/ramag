//! 底部悬浮抽屉：全局热键唤起，仿 Paste.app 横向大卡片墙。
//! 双击卡片 / 数字键 / 回车 → 写回剪贴板并粘贴回原应用。
//! 由 ramag-bin 在 PopUp（NonactivatingPanel）窗口内装载

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

/// 过滤后最多展示的条目数（搜索在全量历史上进行，仅显示前 N 张）
const DRAWER_LIMIT: usize = 60;

/// 全量搜索去抖间隔（与主视图一致）
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
/// 抽屉全量搜索结果上限：展示只取前 60，多取些供去重后仍填满
const DRAWER_SEARCH_LIMIT: usize = 200;

pub struct ClipboardDrawer {
    service: Arc<ClipboardService>,
    /// 最近窗口缓存快照（即时过滤层）
    items: Vec<Arc<ClipItem>>,
    /// 后台全量存储搜索结果（覆盖缓存窗口之外的历史；与主视图同款）
    search_results: Vec<Arc<ClipItem>>,
    search_truncated: bool,
    /// 全量搜索去抖代际：输入变化即自增，旧回包据此丢弃
    search_gen: u64,
    /// 当前全量搜索取消标记；新输入或抽屉关闭时终止旧扫描。
    search_cancel: Arc<AtomicBool>,
    /// 过滤后可见列表上的选中下标
    selected: usize,
    search: Entity<InputState>,
    /// 唤起时记录的平台激活标识，粘贴时恢复原窗口
    activation_target: Option<String>,
    auto_paste: bool,
    /// 粘贴失败提示：渲染时经窗口通知层弹出（此时抽屉保持打开）
    pending_notification: Option<Notification>,
    /// 粘贴进行中：关窗改为异步后防止重复回车触发二次粘贴
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
        // 输入即过滤：内容变化重置选中到首项；回车粘贴当前选中
        subs.push(cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.selected = 0;
                    // 全量搜索：覆盖缓存窗口之外的历史（旧记录也能搜到）
                    this.schedule_search(cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.paste(this.selected, window, cx),
                _ => {}
            },
        ));
        // 搜索框默认聚焦，唤起即可打字过滤
        search.update(cx, |s, cx| s.focus(window, cx));

        // 同步从缓存取最近窗口快照：首帧即满内容，无异步 list 的"先空后填"
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

    /// 取缩略图解密内存图片；缓存命中同步返回，miss 异步解密填充后 notify
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
                    _ => this.img_cache.fail(&path),
                });
            })
            .detach();
        }
        None
    }

    /// 按搜索框内容过滤 + 截断的可见列表（渲染 / 选中 / 粘贴共用同一份）。
    /// 有搜索词时：缓存即时匹配层在前，后台全量结果去重补后（与主视图同款），
    /// 让缓存窗口之外的旧记录也能被搜到
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

    /// 搜索框变化：去抖后台全量搜索，补充缓存窗口之外的匹配（与主视图 schedule_search 同款）
    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        let query = self.search.read(cx).value().to_string();
        self.search_cancel.store(true, Ordering::Relaxed);
        // 去抖等待期间只显示当前关键词的即时匹配，不能混入上一轮后台结果。
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
                        tracing::warn!(error = %e, "drawer full search failed");
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

    /// 粘贴可见列表第 idx 条：写回剪贴板，并按平台恢复原窗口后模拟粘贴。
    /// 成功才关窗——恢复原窗口须在抽屉仍持有前台时执行（Windows 的
    /// SetForegroundWindow 仅对前台进程放行）；失败保持打开并弹提示，不静默
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
                    tracing::warn!(error = %e, "drawer paste failed");
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

    /// 方向键移动选中：过滤后可见列表内边界 clamp，并滚动到可见
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

    /// 键盘：Esc 关闭；↑/↓ 选卡片；主修饰键+1..9 直贴第 N 张。
    /// ←/→ 被搜索框占作光标移动——GPUI 中 action 派发先于按键监听器、拦不住，故选择用上下
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

    /// 顶部工具栏：仿 Paste 居中搜索框
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

        // 先取 owned 颜色释放 theme 借用，否则与下方 render_card 的 &mut cx 冲突
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let focus = self.focus_handle.clone();

        let (visible, truncated) = self.visible_items_with_status(cx);
        // 过滤后列表变短时把选中夹回范围内
        if self.selected >= visible.len() {
            self.selected = visible.len().saturating_sub(1);
        }
        let empty = visible.is_empty();

        let topbar = self.render_topbar(truncated).into_any_element();
        // for 循环（非 map 闭包）：render_card 需 &mut Context，闭包会触发借用逃逸
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
