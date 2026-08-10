mod card;
mod detail;
mod ops;
mod render;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, SharedString, Subscription,
    UniformListScrollHandle, Window,
};
use gpui_component::input::{InputEvent, InputState};
use ramag_app::ClipboardService;
use ramag_domain::entities::{ClipId, ClipItem, ClipKind, ClipboardSettings};

/// 采集结果的界面刷新间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(600);

pub struct ClipboardView {
    pub(super) service: Arc<ClipboardService>,
    pub(super) items: Vec<Arc<ClipItem>>,
    pub(super) settings: ClipboardSettings,
    pub(super) loaded_settings_revision: u64,
    pub(super) search: Entity<InputState>,
    pub(super) filter: Option<ClipKind>,
    pub(super) selected: Option<ClipId>,
    /// 当前详情文本的有界展示缓存；避免轮询重绘反复复制、排版大文本。
    pub(super) detail_text_cache: Option<(ClipId, SharedString)>,
    pub(super) loaded_revision: u64,
    pub(super) search_results: Vec<Arc<ClipItem>>,
    /// 后台搜索命中超过 SEARCH_LIMIT；状态栏必须明确提示结果被截断。
    pub(super) search_truncated: bool,
    /// 用于丢弃过期搜索结果。
    pub(super) search_gen: u64,
    /// 当前全量搜索的取消标记；输入变化或视图销毁时停止旧扫描。
    pub(super) search_cancel: Arc<AtomicBool>,
    pub(super) list_scroll: UniformListScrollHandle,
    pub(super) focus_handle: FocusHandle,
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    pub(super) img_cache: crate::views::image_cache::ImageCache,
    /// 首次显示时聚焦搜索框（仅一次，不抢用户后续焦点）
    pub(super) focused_search_once: bool,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for ClipboardView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for ClipboardView {
    fn drop(&mut self) {
        self.search_cancel.store(true, Ordering::Relaxed);
    }
}

impl ClipboardView {
    pub fn new(
        service: Arc<ClipboardService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("搜索剪贴历史…"));

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe(&search, |this: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.schedule_search(cx);
                    cx.notify();
                }
            }),
        );

        let (settings, loaded_settings_revision) = service.settings_snapshot_with_revision();
        let mut view = Self {
            service,
            items: Vec::new(),
            settings,
            loaded_settings_revision,
            search,
            filter: None,
            selected: None,
            detail_text_cache: None,
            loaded_revision: 0,
            search_results: Vec::new(),
            search_truncated: false,
            search_gen: 0,
            search_cancel: Arc::new(AtomicBool::new(false)),
            focused_search_once: false,
            list_scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            pending_notification: None,
            img_cache: crate::views::image_cache::ImageCache::new(),
            _subscriptions: subscriptions,
        };
        view.load_settings(cx);
        view.reload(cx);
        view.start_polling(cx);
        view
    }

    /// 仅在版本变化时同步缓存快照。
    fn start_polling(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.service.revision() != this.loaded_revision {
                            this.reload(cx);
                        }
                        let settings_revision = this.service.settings_revision();
                        if settings_revision != this.loaded_settings_revision {
                            let (settings, revision) =
                                this.service.settings_snapshot_with_revision();
                            this.settings = settings;
                            this.loaded_settings_revision = revision;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }
}
