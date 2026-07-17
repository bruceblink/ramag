//! ClipboardView：剪贴板历史主视图。左卡片流 + 右详情，搜索 / 类型筛选。
//! 历史由 App 级采集循环写入 storage；视图轮询 `service.revision()` 仅在变化时重载

mod card;
mod detail;
mod ops;
mod render;
mod settings;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, Subscription,
    UniformListScrollHandle, Window,
};
use gpui_component::input::{InputEvent, InputState};
use ramag_app::ClipboardService;
use ramag_domain::entities::{ClipId, ClipItem, ClipKind, ClipboardSettings};

/// 轮询间隔：采集循环写库后，视图最迟此间隔内刷新
const POLL_INTERVAL: Duration = Duration::from_millis(600);

pub struct ClipboardView {
    pub(super) service: Arc<ClipboardService>,
    /// 最近窗口快照（来自 service 缓存，最近优先）
    pub(super) items: Vec<Arc<ClipItem>>,
    pub(super) settings: ClipboardSettings,
    pub(super) loaded_settings_revision: u64,
    pub(super) settings_save_generation: u64,
    pub(super) settings_saving: bool,
    pub(super) search: Entity<InputState>,
    /// 类型筛选；None = 全部
    pub(super) filter: Option<ClipKind>,
    pub(super) selected: Option<ClipId>,
    /// 上次已加载的版本号，轮询时与 service.revision() 比对
    pub(super) loaded_revision: u64,
    /// 后台全量搜索结果：补充缓存窗口之外的匹配（搜索词非空时与缓存即时结果合并）
    pub(super) search_results: Vec<Arc<ClipItem>>,
    /// 后台搜索命中超过 SEARCH_LIMIT；状态栏必须明确提示结果被截断。
    pub(super) search_truncated: bool,
    /// 搜索去抖代号：每次输入自增，异步任务到期比对以丢弃过期搜索
    pub(super) search_gen: u64,
    /// 当前全量搜索的取消标记；输入变化或视图销毁时停止旧扫描。
    pub(super) search_cancel: Arc<AtomicBool>,
    /// 设置面板是否展开
    pub(super) show_settings: bool,
    pub(super) list_scroll: UniformListScrollHandle,
    pub(super) focus_handle: FocusHandle,
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    /// 图片解密缓存（缩略图 / 原图）
    pub(super) img_cache: crate::views::image_cache::ImageCache,
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
    pub fn show_settings(&mut self, cx: &mut Context<Self>) {
        self.show_settings = true;
        cx.notify();
    }

    pub fn new(
        service: Arc<ClipboardService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("搜索剪贴历史…"));

        let mut subscriptions = Vec::new();
        // 搜索框输入即重渲染（过滤是纯内存操作）
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
            settings_save_generation: 0,
            settings_saving: false,
            search,
            filter: None,
            selected: None,
            loaded_revision: 0,
            search_results: Vec::new(),
            search_truncated: false,
            search_gen: 0,
            search_cancel: Arc::new(AtomicBool::new(false)),
            show_settings: false,
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

    /// 后台计时轮询：版本号变化才重载（重载只同步拷贝缓存快照，不解密）
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
                        if !this.settings_saving
                            && settings_revision != this.loaded_settings_revision
                        {
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
