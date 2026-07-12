//! 查询历史弹框内容：当前连接的最近查询记录（最近优先，最多 200 条）。
//! 行操作：复制 SQL / 填入编辑器（不自动执行）；入口在 QueryPanel 工具条，
//! 由 QueryPanel 经 `window.open_dialog` 装载本视图并订阅 `HistoryEvent`

use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, EventEmitter, Hsla, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    scroll::ScrollableElement as _,
    v_flex,
};
use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionId, QueryRecord, QueryStatus};
use tracing::error;

/// 单次最多加载条数
const HISTORY_LIMIT: usize = 200;
/// SQL 单行预览最大字符数
const PREVIEW_MAX_CHARS: usize = 160;
/// 失败原因展示最大字符数
const ERROR_MAX_CHARS: usize = 80;
/// 列表区固定高度：加载前后弹框尺寸不跳动，超出部分内部滚动
const LIST_HEIGHT: f32 = 480.0;

/// 用户点了「填入编辑器」：QueryPanel 订阅后写入当前活动 Tab（不执行）
pub enum HistoryEvent {
    FillEditor(String),
}

/// 弹框内容视图：异步加载 + 列表渲染
pub struct HistoryList {
    service: Arc<ConnectionService>,
    /// 只看当前连接的历史（list_history 原生支持按 ConnectionId 过滤）
    connection_id: ConnectionId,
    records: Vec<QueryRecord>,
    loading: bool,
    error: Option<String>,
}

impl EventEmitter<HistoryEvent> for HistoryList {}

impl HistoryList {
    pub fn new(
        service: Arc<ConnectionService>,
        connection_id: ConnectionId,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            service,
            connection_id,
            records: Vec::new(),
            loading: true,
            error: None,
        };
        this.load(cx);
        this
    }

    /// 异步拉取历史。必须走 service（内部已处理 storage 的线程桥接），视图层不碰底层
    fn load(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        let svc = self.service.clone();
        let conn_id = self.connection_id.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.list_history(Some(&conn_id), HISTORY_LIMIT).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(rs) => this.records = rs,
                    Err(e) => {
                        error!(error = %e, "load query history failed");
                        this.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 单行：状态点 + SQL 预览 + 元信息（时间 / 行数 / 耗时或失败原因）+ 行操作
    fn render_row(&self, ix: usize, rec: QueryRecord, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let muted_bg = theme.muted;
        let status_color = match rec.status {
            QueryStatus::Success => theme.success,
            QueryStatus::Failed => theme.danger,
        };

        let preview = rec.sql_preview(PREVIEW_MAX_CHARS);
        let when_text = rec
            .executed_at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let meta = match rec.status {
            QueryStatus::Success => {
                format!("{} · {} 行 · {} ms", when_text, rec.rows, rec.elapsed_ms)
            }
            QueryStatus::Failed => format!(
                "{} · 失败：{}",
                when_text,
                compact_truncate(rec.error.as_deref().unwrap_or("未知错误"), ERROR_MAX_CHARS)
            ),
        };
        let sql_for_copy = rec.sql.clone();
        let sql_for_fill = rec.sql;

        h_flex()
            .id(SharedString::from(format!("hist-row-{ix}")))
            .items_center()
            .gap_3()
            .px_3()
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .hover(move |s| s.bg(muted_bg))
            // 状态点：成功绿 / 失败红
            .child(
                div()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded_full()
                    .bg(status_color)
                    .flex_none(),
            )
            // 文本块：SQL 预览 + 元信息，各自单行截断
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(preview),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(meta),
                    ),
            )
            // 行操作
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("hist-copy-{ix}")))
                            .ghost()
                            .xsmall()
                            .label("复制")
                            .tooltip("复制 SQL 到剪贴板")
                            .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    sql_for_copy.clone(),
                                ));
                                window.push_notification(
                                    Notification::success("已复制 SQL").autohide(true),
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("hist-fill-{ix}")))
                            .ghost()
                            .xsmall()
                            .label("填入编辑器")
                            .tooltip("填入当前查询 Tab 的编辑器（不执行）")
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.emit(HistoryEvent::FillEditor(sql_for_fill.clone()));
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for HistoryList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let danger = theme.danger;

        let body: AnyElement = if self.loading {
            centered_hint("加载中…", muted_fg)
        } else if let Some(e) = &self.error {
            centered_hint(format!("加载失败：{e}"), danger)
        } else if self.records.is_empty() {
            centered_hint("暂无查询历史", muted_fg)
        } else {
            let mut rows: Vec<AnyElement> = Vec::with_capacity(self.records.len());
            for (ix, rec) in self.records.clone().into_iter().enumerate() {
                rows.push(self.render_row(ix, rec, cx));
            }
            div()
                .size_full()
                .overflow_y_scrollbar()
                .child(v_flex().w_full().children(rows))
                .into_any_element()
        };

        // 外层给定高度，内层 size_full + overflow 才能滚（同 cli_console 模式）
        div().w_full().h(px(LIST_HEIGHT)).child(body)
    }
}

/// 居中提示（加载中 / 空态 / 错误共用）
fn centered_hint(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

/// 压平空白并按字符数截断（多字节安全），超长补省略号
fn compact_truncate(s: &str, max_chars: usize) -> String {
    let normalized: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let truncated: String = normalized.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::compact_truncate;

    #[test]
    fn short_text_passes_through() {
        assert_eq!(compact_truncate("SELECT 1;", 20), "SELECT 1;");
    }

    #[test]
    fn long_text_cut_by_chars_with_ellipsis() {
        assert_eq!(compact_truncate("abcdef", 3), "abc…");
        // 多字节字符按字符数截断，不得 panic
        assert_eq!(compact_truncate("数据库查询失败", 3), "数据库…");
    }

    #[test]
    fn whitespace_is_flattened() {
        assert_eq!(compact_truncate("a\n  b\t c", 20), "a b c");
    }
}
