//! 查询结果差异对话框：使用 Git 风格的行标记展示已加载范围内的变化。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Render, ScrollHandle, Styled,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, Theme,
    button::ButtonVariants as _,
    h_flex,
    scroll::{Scrollbar, ScrollbarShow},
    spinner::Spinner,
    v_flex,
};

use super::result_diff::{
    ResultDiff, ResultDiffKind, ResultDiffLine, ResultSnapshot, RowMatchMode, build_result_diff,
    format_result_diff,
};

const DIFF_VIEW_WIDTH: f32 = 1_080.0;
const DIFF_VIEW_HEIGHT: f32 = 540.0;

pub(crate) struct ResultDiffDialog {
    source: ResultSnapshot,
    target: ResultSnapshot,
    diff: Option<ResultDiff>,
    loading: bool,
    request_generation: u64,
    error: Option<String>,
    vertical_scroll: ScrollHandle,
    horizontal_scroll: ScrollHandle,
}

impl ResultDiffDialog {
    pub(crate) fn new(
        source: ResultSnapshot,
        target: ResultSnapshot,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            source,
            target,
            diff: None,
            loading: false,
            request_generation: 0,
            error: None,
            vertical_scroll: ScrollHandle::new(),
            horizontal_scroll: ScrollHandle::new(),
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let request_generation = self.request_generation;
        let source = self.source.clone();
        let target = self.target.clone();
        self.loading = true;
        self.diff = None;
        self.error = None;
        self.vertical_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        self.horizontal_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome =
                ramag_app::run_blocking(move || Ok(build_result_diff(&source, &target))).await;
            let _ = this.update(cx, |this, cx| {
                if this.request_generation != request_generation {
                    return;
                }
                this.loading = false;
                match outcome {
                    Ok(diff) => this.diff = Some(diff),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_diff_line(&self, line: &ResultDiffLine, theme: &Theme) -> impl IntoElement {
        let (prefix, color, background) = match line.kind {
            ResultDiffKind::Context => (' ', theme.muted_foreground, theme.background),
            ResultDiffKind::Added => ('+', theme.success, theme.success.opacity(0.08)),
            ResultDiffKind::Removed => ('-', theme.danger, theme.danger.opacity(0.08)),
        };
        h_flex()
            .w(px(DIFF_VIEW_WIDTH))
            .items_start()
            .gap(px(6.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded_sm()
            .bg(background)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .whitespace_nowrap()
            .child(
                div()
                    .flex_none()
                    .w(px(14.0))
                    .text_color(color)
                    .child(prefix.to_string()),
            )
            .child(div().flex_none().text_color(color).child(line.text.clone()))
    }

    fn render_section(
        &self,
        title: &str,
        lines: &[ResultDiffLine],
        omitted_lines: usize,
        theme: &Theme,
    ) -> AnyElement {
        let mut body = v_flex()
            .w(px(DIFF_VIEW_WIDTH))
            .gap(px(2.0))
            .p(px(8.0))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title.to_string()),
            );
        if lines.is_empty() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("无差异"),
            );
        } else {
            for line in lines {
                body = body.child(self.render_diff_line(line, theme));
            }
            if omitted_lines > 0 {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("… 还有 {omitted_lines} 行未显示")),
                );
            }
        }
        body.into_any_element()
    }

    fn render_summary(&self, diff: &ResultDiff, theme: &Theme) -> AnyElement {
        let status_color = if diff.has_changes() {
            theme.warning
        } else {
            theme.success
        };
        let status = if diff.has_changes() {
            "发现差异"
        } else {
            "结果一致"
        };
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(14.0))
            .text_xs()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(status_color)
                    .child(status),
            )
            .child(div().text_color(theme.muted_foreground).child(format!(
                "字段 修改 {} · 新增 {} · 删除 {}",
                diff.columns_changed, diff.columns_added, diff.columns_removed
            )))
            .child(div().text_color(theme.muted_foreground).child(format!(
                "行 变更 {} · 新增 {} · 删除 {} · 未变化 {}",
                diff.rows_changed, diff.rows_added, diff.rows_removed, diff.rows_unchanged
            )))
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("匹配：{}", diff.row_mode.label())),
            )
            .into_any_element()
    }

    fn render_notes(&self, diff: &ResultDiff, theme: &Theme) -> Option<AnyElement> {
        let mut notes = Vec::new();
        if diff.context_mismatch {
            notes.push("两次结果的连接、SQL 或表目标不同，差异只代表当前已加载内容".to_string());
        }
        if diff.scope_mismatch {
            notes.push("两次结果的分页或截断范围不同，不能据此判断完整结果集差异".to_string());
        }
        if diff.row_mode == RowMatchMode::Content {
            notes.push("未找到两侧都存在的稳定键；行修改可能显示为一条删除和一条新增".to_string());
        } else if diff.row_mode == RowMatchMode::Unavailable {
            notes.push("两次结果没有共有列，因此未比较行内容".to_string());
        }
        if diff.source_rows_compared < diff.source_rows
            || diff.target_rows_compared < diff.target_rows
        {
            notes.push(format!(
                "每侧最多比较 {} 行，超出部分未参与差异计算",
                super::result_diff::MAX_COMPARE_ROWS
            ));
        }
        if diff.unkeyed_source_rows > 0 || diff.unkeyed_target_rows > 0 {
            notes.push(format!(
                "{} 个基准行、{} 个当前行缺少可用键值，按未匹配行处理",
                diff.unkeyed_source_rows, diff.unkeyed_target_rows
            ));
        }
        if diff.omitted_column_lines > 0 || diff.omitted_row_lines > 0 {
            notes.push(format!(
                "差异行较多，已省略 {} 个字段行和 {} 个数据行；复制内容也遵循此显示上限",
                diff.omitted_column_lines, diff.omitted_row_lines
            ));
        }
        if notes.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .w_full()
                .flex_none()
                .gap(px(2.0))
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(theme.warning.opacity(0.08))
                .children(
                    notes
                        .into_iter()
                        .map(|note| div().text_xs().text_color(theme.warning).child(note)),
                )
                .into_any_element(),
        )
    }

    fn render_scrollable(&self, content: impl IntoElement, theme: &Theme) -> AnyElement {
        div()
            .relative()
            .h(px(DIFF_VIEW_HEIGHT))
            .w_full()
            .child(
                div()
                    .id("result-diff-horizontal-scroll")
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.horizontal_scroll)
                    .child(
                        div()
                            .id("result-diff-vertical-scroll")
                            .w(px(DIFF_VIEW_WIDTH))
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.vertical_scroll)
                            .child(content),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom(px(16.0))
                    .right_0()
                    .w(px(16.0))
                    .bg(theme.scrollbar)
                    .child(
                        Scrollbar::vertical(&self.vertical_scroll)
                            .id("result-diff-vertical-scrollbar")
                            .scrollbar_show(ScrollbarShow::Always),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(16.0))
                    .bg(theme.scrollbar)
                    .child(
                        Scrollbar::horizontal(&self.horizontal_scroll)
                            .id("result-diff-horizontal-scrollbar")
                            .scrollbar_show(ScrollbarShow::Always),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ResultDiffDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let copy_text = self
            .diff
            .as_ref()
            .map(|diff| format_result_diff(&self.source, &self.target, diff))
            .unwrap_or_default();
        let has_diff = self.diff.is_some();
        let toolbar = h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("结果差异"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "基准：{} · {}",
                                self.source.label, self.source.scope
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "当前：{} · {}",
                                self.target.label, self.target.scope
                            )),
                    ),
            )
            .child(
                ramag_ui::clickable_button("result-diff-copy")
                    .ghost()
                    .small()
                    .icon(IconName::Copy)
                    .tooltip("复制差异")
                    .disabled(self.loading || !has_diff)
                    .on_click(move |_: &ClickEvent, window, app| {
                        ramag_ui::copy_text_with_notification(copy_text.clone(), window, app);
                    }),
            )
            .child(
                ramag_ui::clickable_button("result-diff-refresh")
                    .ghost()
                    .small()
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("重新比较")
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
            );

        let body: AnyElement = if self.loading {
            v_flex()
                .h(px(DIFF_VIEW_HEIGHT))
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().small())
                .child("正在比较当前已加载结果…")
                .into_any_element()
        } else if let Some(error) = &self.error {
            v_flex()
                .h(px(DIFF_VIEW_HEIGHT))
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .text_xs()
                .text_color(theme.danger)
                .child(error.clone())
                .into_any_element()
        } else if let Some(diff) = self.diff.as_ref() {
            let mut content = v_flex()
                .w(px(DIFF_VIEW_WIDTH))
                .gap(px(8.0))
                .child(self.render_summary(diff, theme));
            if let Some(notes) = self.render_notes(diff, theme) {
                content = content.child(notes);
            }
            content = content
                .child(self.render_section(
                    "字段变化",
                    &diff.column_lines,
                    diff.omitted_column_lines,
                    theme,
                ))
                .child(self.render_section(
                    "行变化",
                    &diff.row_lines,
                    diff.omitted_row_lines,
                    theme,
                ));
            self.render_scrollable(content, theme)
        } else {
            v_flex()
                .h(px(DIFF_VIEW_HEIGHT))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("暂无差异结果")
                .into_any_element()
        };

        v_flex().w_full().gap(px(8.0)).child(toolbar).child(body)
    }
}
