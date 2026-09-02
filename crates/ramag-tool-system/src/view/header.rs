//! 系统监控标题栏及刷新操作的响应式布局。

use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};

use super::{SystemSection, SystemView};
use crate::RefreshInterval;

impl SystemView {
    /// 根据窗口宽度重排标题和操作区，窄窗口仍保留刷新频率与立即刷新操作。
    pub(super) fn render_header(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let interval = self.monitor.refresh_interval();
        let mut tabs = h_flex().gap(px(4.0));
        tabs = tabs.child(self.section_button(
            SystemSection::Performance,
            "性能监控",
            ramag_ui::icons::gauge(),
            cx,
        ));
        tabs = tabs.child(self.section_button(
            SystemSection::Processes,
            "任务管理器",
            Icon::new(IconName::MemoryStick),
            cx,
        ));

        let mut intervals = h_flex().gap(px(2.0));
        for option in [
            RefreshInterval::OneSecond,
            RefreshInterval::TwoSeconds,
            RefreshInterval::FiveSeconds,
        ] {
            let selected = option == interval;
            let label = option.label();
            let mut button = ramag_ui::clickable_button(SharedString::from(format!(
                "system-interval-{}",
                label
            )))
            .xsmall()
            .label(label);
            button = if selected {
                button.primary()
            } else {
                button.ghost()
            };
            intervals = intervals.child(button.on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.select_interval(option, cx)),
            ));
        }
        let theme = cx.theme();

        if window.viewport_size().width < px(720.0) {
            return v_flex()
                .debug_selector(|| "system-header".to_owned())
                .w_full()
                .flex_none()
                .gap(px(8.0))
                .px_4()
                .py(px(10.0))
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(px(10.0))
                        .child(ramag_ui::icons::gauge().text_color(theme.accent))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(1.0))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("系统监控"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("本机性能与运行中进程"),
                                ),
                        ),
                )
                .child(
                    h_flex()
                        .debug_selector(|| "system-header-controls".to_owned())
                        .w_full()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.0))
                        .child(tabs)
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .flex_wrap()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("刷新 {}", interval.status_label())),
                                )
                                .child(intervals)
                                .child(
                                    ramag_ui::clickable_button("system-refresh")
                                        .ghost()
                                        .small()
                                        .icon(ramag_ui::icons::refresh_cw())
                                        .tooltip("立即刷新")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.refresh_now(cx);
                                        })),
                                ),
                        ),
                )
                .into_any_element();
        }

        h_flex()
            .debug_selector(|| "system-header".to_owned())
            .w_full()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .px_4()
            .py(px(10.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(10.0))
                    .child(ramag_ui::icons::gauge().text_color(theme.accent))
                    .child(
                        v_flex()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("系统监控"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("本机性能与运行中进程"),
                            ),
                    )
                    .child(tabs),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("刷新 {}", interval.status_label())),
                    )
                    .child(intervals)
                    .child(
                        ramag_ui::clickable_button("system-refresh")
                            .ghost()
                            .small()
                            .icon(ramag_ui::icons::refresh_cw())
                            .tooltip("立即刷新")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.refresh_now(cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}
