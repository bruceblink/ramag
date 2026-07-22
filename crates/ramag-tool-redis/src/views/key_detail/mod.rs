//! Key 详情：按 RedisValue variant dispatch 到 scalar / list_block / hash_block / set_block / zset_block / stream_block

mod hash_block;
mod header;
mod helpers;
mod list_block;
mod ops;
#[cfg(test)]
mod render_test;
mod scalar;
mod set_block;
mod stream_block;
mod zset_block;

use std::sync::Arc;

use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    ScrollStrategy, SharedString, Styled, UniformListScrollHandle, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _, notification::Notification,
    scroll::ScrollableElement as _, v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{ConnectionConfig, MAX_REDIS_COLLECTION_ITEMS, RedisValue};

use helpers::render_value;

use crate::views::value_display::ViewMode;

const MAX_COLLECTION_ITEMS: usize = MAX_REDIS_COLLECTION_ITEMS;

#[derive(Debug, Clone)]
pub enum KeyDetailEvent {
    /// 详情面板触发 DEL 后通知 KeyTree 刷新
    Deleted(String),
    /// 请求编辑 TTL，弹窗由上层 Session 处理。(key, 当前 ttl_ms)
    RequestEditTtl(String, Option<i64>),
    /// 请求编辑 String 值（弹窗由上层 Session 处理）
    /// (key 名, 当前文本值)
    RequestEditValue(String, String),
    /// 请求新增 Hash 字段（弹窗）
    /// (key 名)
    RequestAddHashField(String),
    /// 请求编辑 Hash 字段（弹窗）
    /// (key 名, field, 当前 value 文本预览)
    RequestEditHashField(String, String, String),
    /// 请求新增 List 元素（弹窗）
    /// (key 名)
    RequestAddListElement(String),
    /// 请求新增 Set 元素（弹窗）
    /// (key 名)
    RequestAddSetElement(String),
    /// 请求新增 ZSet 元素（弹窗）
    /// (key 名)
    RequestAddZSetElement(String),
    /// 请求编辑 ZSet 成员的 score（弹窗）
    /// (key 名, member, 当前 score 字符串)
    RequestEditZSetScore(String, String, String),
    /// 请求新增 Stream 条目（弹窗）
    /// (key 名)
    RequestAddStreamEntry(String),
    /// 请求删除 Key（由上层 Session 弹二次确认）
    /// (key 名)
    RequestDeleteKey(String),
    /// 请求删除 Hash 字段（由上层 Session 弹二次确认）
    /// (key 名, field 名)
    RequestDeleteHashField(String, String),
    /// 请求删除 List 元素（由上层 Session 弹二次确认）
    /// (key 名, 元素值, 序号)
    RequestDeleteListElement(String, String, usize),
    /// 请求删除 Set 成员（由上层 Session 弹二次确认）
    /// (key 名, 成员值)
    RequestDeleteSetElement(String, String),
    /// 请求删除 ZSet 成员（由上层 Session 弹二次确认）
    /// (key 名, 成员值)
    RequestDeleteZSetMember(String, String),
    /// 请求删除 Stream 条目（由上层 Session 弹二次确认）
    /// (key 名, entry_id)
    RequestDeleteStreamEntry(String, String),
}

pub struct KeyDetailPanel {
    service: Arc<RedisService>,
    config: Option<ConnectionConfig>,
    /// pub(super) 让 header 模块读 db / value / ttl_ms / 大小估算状态
    pub(super) db: u8,
    /// 当前 key 名（None = 未选中任何 key）
    key: Option<String>,
    /// 当前 key 的值（fetch 后填充）
    pub(super) value: Option<RedisValue>,
    /// TTL 毫秒（-1 永久 / -2 不存在 / >=0 剩余）
    pub(super) ttl_ms: Option<i64>,
    pub(super) ttl_loading: bool,
    /// PTTL 局部错误；值加载成功时不应因 TTL 失败隐藏整个 key。
    pub(super) ttl_error: Option<String>,
    /// 加载状态
    loading: bool,
    /// 详情读取代际；同一 key 的旧值/TTL/大小回包也不能覆盖较新的刷新。
    request_seq: u64,
    error: Option<String>,
    /// 写操作的 toast（只读拦截 / 删除失败等，render 时 push，不覆盖内容区）
    pending_notification: Option<Notification>,
    /// 单 Key 字节估算（MEMORY USAGE，需用户主动点击触发）
    pub(super) key_size_bytes: Option<u64>,
    pub(super) estimating_size: bool,
    /// MEMORY USAGE 的局部错误；不得覆盖已成功加载的 key 内容。
    pub(super) size_error: Option<String>,
    /// 集合服务端总数；None 表示标量或尚未加载。
    pub(super) collection_total: Option<u64>,
    /// 多批集合读取因累计内容字节预算只保留了安全前缀。
    pub(super) value_byte_limited: bool,
    /// 当前值已达到交互结果内存提示线。
    pub(super) value_memory_warning: bool,
    /// 标量值视图模式：None=按内容自动（JSON 美化 / Raw），Some=用户手动选定
    value_view_mode: Option<ViewMode>,
    /// 标量渲染缓存：(请求的 view_mode, 生效 mode, 按行切好的内容, gzip 提示)。
    /// 行数组供 uniform_list 行级虚拟化（大值单文本节点渲染会卡死滚动）；
    /// 解压 + JSON 解析 + 切行只算一次，key/value/view_mode 变化失效
    #[allow(clippy::type_complexity)]
    pub(super) scalar_cache: std::cell::RefCell<
        Option<(
            Option<ViewMode>,
            ViewMode,
            std::sync::Arc<Vec<SharedString>>,
            Option<SharedString>,
        )>,
    >,
    /// Session 调 focus_panel 聚焦后，cmd-w 等 action 走焦点链路由到 Session
    focus_handle: FocusHandle,
    /// 容器值（hash/list/set/zset/stream）uniform_list 行级虚拟化的滚动句柄
    value_scroll: UniformListScrollHandle,
    /// 标量内容区横向滚动句柄（内容固定宽 + 外层 X 滚动，行尾不被视口裁掉）
    pub(super) scalar_h_scroll: gpui::ScrollHandle,
}

impl EventEmitter<KeyDetailEvent> for KeyDetailPanel {}

impl Focusable for KeyDetailPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl KeyDetailPanel {
    pub fn new(service: Arc<RedisService>, cx: &mut Context<Self>) -> Self {
        Self {
            service,
            config: None,
            db: 0,
            key: None,
            value: None,
            ttl_ms: None,
            ttl_loading: false,
            ttl_error: None,
            loading: false,
            request_seq: 0,
            error: None,
            pending_notification: None,
            key_size_bytes: None,
            size_error: None,
            collection_total: None,
            value_byte_limited: false,
            value_memory_warning: false,
            value_view_mode: None,
            scalar_cache: std::cell::RefCell::new(None),
            focus_handle: cx.focus_handle(),
            estimating_size: false,
            value_scroll: UniformListScrollHandle::new(),
            scalar_h_scroll: gpui::ScrollHandle::new(),
        }
    }

    pub fn set_connection(
        &mut self,
        config: Option<ConnectionConfig>,
        db: u8,
        cx: &mut Context<Self>,
    ) {
        self.request_seq = self.request_seq.wrapping_add(1);
        self.config = config;
        self.db = db;
        self.key = None;
        self.value = None;
        self.ttl_ms = None;
        self.ttl_loading = false;
        self.ttl_error = None;
        self.loading = false;
        self.error = None;
        self.key_size_bytes = None;
        self.estimating_size = false;
        self.size_error = None;
        self.collection_total = None;
        self.value_byte_limited = false;
        self.value_memory_warning = false;
        self.value_view_mode = None;
        *self.scalar_cache.borrow_mut() = None;
        // 换 key 后滚动归顶：uniform_list 句柄跨 key 复用，不复位会残留上个 key 的偏移
        self.value_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.scalar_h_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        cx.notify();
    }

    /// 当前正在展示的 key 名（None = 未选中任何 key）
    pub fn current_key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// 清空当前展示（恢复"未选中 Key"占位态）
    pub fn clear_key(&mut self, cx: &mut Context<Self>) {
        self.request_seq = self.request_seq.wrapping_add(1);
        self.key = None;
        self.value = None;
        self.ttl_ms = None;
        self.ttl_loading = false;
        self.ttl_error = None;
        self.loading = false;
        self.error = None;
        self.key_size_bytes = None;
        self.estimating_size = false;
        self.size_error = None;
        self.collection_total = None;
        self.value_byte_limited = false;
        self.value_memory_warning = false;
        self.value_view_mode = None;
        *self.scalar_cache.borrow_mut() = None;
        // 换 key 后滚动归顶：uniform_list 句柄跨 key 复用，不复位会残留上个 key 的偏移
        self.value_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.scalar_h_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        cx.notify();
    }

    /// 把整面板焦点拿到（Session 在初始化 / 选中 key 时调）
    pub fn focus_panel(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// scalar 视图模式切换按钮调用：固定为用户选择的模式（覆盖按内容自动选择）
    pub(super) fn set_value_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if self.value_view_mode != Some(mode) {
            self.value_view_mode = Some(mode);
            cx.notify();
        }
    }
}

impl Render for KeyDetailPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let bg = theme.background;
        let accent = theme.accent;

        let Some(key) = self.key.clone() else {
            return v_flex()
                .size_full()
                .bg(bg)
                .track_focus(&self.focus_handle)
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted_fg)
                        .child("从左侧选择一个 Key 查看详情"),
                )
                .into_any_element();
        };

        let header = header::render_header(self, &key, fg, muted_fg, accent, border, cx);
        let view_mode = self.value_view_mode;

        // body + 是否自带虚拟滚动：容器类型走 uniform_list（自滚动），其余走普通滚动
        let (body, self_scrolls): (gpui::AnyElement, bool) = if self.loading {
            (
                div()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(muted_fg)
                    .child("加载中…")
                    .into_any_element(),
                false,
            )
        } else if let Some(err) = self.error.clone() {
            let key_for_retry = key.clone();
            (
                v_flex()
                    .p(px(14.0))
                    .gap_2()
                    .items_start()
                    .child(div().text_sm().text_color(gpui::red()).child(err))
                    .child(
                        ramag_ui::clickable_button("redis-key-load-retry")
                            .outline()
                            .small()
                            .label("重试")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.load_key(key_for_retry.clone(), cx);
                            })),
                    )
                    .into_any_element(),
                false,
            )
        } else {
            match &self.value {
                // 容器类型：helpers::render_value 内部用 uniform_list 行级虚拟化（自带滚动）
                Some(
                    v @ (RedisValue::List(_)
                    | RedisValue::Hash(_)
                    | RedisValue::Set(_)
                    | RedisValue::ZSet(_)
                    | RedisValue::Stream(_)),
                ) => (
                    render_value(v, &key, cx, &self.value_scroll, fg, muted_fg, border),
                    true,
                ),
                // 标量 String/Bytes 走 scalar 模块（含 Gzip 提示 + 编辑按钮）；
                // 内容区 uniform_list 行级虚拟化自带滚动（大值整体滚动会卡死）
                Some(v @ (RedisValue::Text(_) | RedisValue::Bytes(_))) => (
                    scalar::render_scalar(
                        self,
                        &key,
                        v,
                        view_mode,
                        &self.value_scroll,
                        fg,
                        muted_fg,
                        border,
                        cx,
                        window,
                    )
                    .into_any_element(),
                    true,
                ),
                // Nil/Int/Float/Bool/Array：小体量，普通渲染
                Some(v) => (
                    render_value(v, &key, cx, &self.value_scroll, fg, muted_fg, border),
                    false,
                ),
                None => (
                    div()
                        .p(px(14.0))
                        .text_sm()
                        .text_color(muted_fg)
                        .child("(无值)")
                        .into_any_element(),
                    false,
                ),
            }
        };

        // 滚动区：外层 flex_1 + min_h_0 给出「减去 header 后的确定高度」。
        // 容器类型的 uniform_list 自带虚拟滚动，只需内边距；其余套 overflow_y_scrollbar。
        let content = if self_scrolls {
            div().flex_col().flex_1().min_h_0().p(px(14.0)).child(body)
        } else {
            div().flex_1().min_h_0().child(
                div()
                    .size_full()
                    .overflow_y_scrollbar()
                    .child(div().w_full().p(px(14.0)).child(body)),
            )
        };

        v_flex()
            .size_full()
            .bg(bg)
            .track_focus(&self.focus_handle)
            .child(header)
            .child(content)
            .into_any_element()
    }
}
