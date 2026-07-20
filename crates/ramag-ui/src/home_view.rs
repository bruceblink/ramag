//! 首页：ANSI Shadow Logo + tagline + 工具入口卡片（与左侧 ActivityBar 同源）

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, hsla, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};

use ramag_app::ToolRegistry;

#[derive(Debug, Clone)]
pub enum HomeEvent {
    OpenTool(String),
}

/// ANSI Shadow 大字，等宽对齐
const RAMAG_LOGO: &[&str] = &[
    "██████╗  █████╗ ███╗   ███╗ █████╗  ██████╗ ",
    "██╔══██╗██╔══██╗████╗ ████║██╔══██╗██╔════╝ ",
    "██████╔╝███████║██╔████╔██║███████║██║  ███╗",
    "██╔══██╗██╔══██║██║╚██╔╝██║██╔══██║██║   ██║",
    "██║  ██║██║  ██║██║ ╚═╝ ██║██║  ██║╚██████╔╝",
    "╚═╝  ╚═╝╚═╝  ╚═╝╚═╝     ╚═╝╚═╝  ╚═╝ ╚═════╝ ",
];

pub struct HomeView {
    registry: Arc<ToolRegistry>,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

impl Render for HomeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let mono = theme.mono_font_family.clone();
        let bg = theme.background;
        let border = theme.border;
        let fg = theme.foreground;
        let card_bg = theme.secondary;

        let mut accent_border = accent;
        accent_border.a = 0.55;

        let cards = self.registry.list().into_iter().map(|tool| {
            let id = tool.meta().id.clone();
            let card_id = SharedString::from(format!("home-tool-{id}"));
            let name = tool.meta().name.clone();
            let description = tool.meta().description.clone();
            let icon = crate::activity_bar::ActivityBar::icon_for_tool(&id);

            v_flex()
                .id(card_id)
                .w(px(280.0))
                .p(px(20.0))
                .gap(px(10.0))
                .bg(card_bg)
                .border_1()
                .border_color(border)
                .rounded(px(10.0))
                .cursor_pointer()
                .hover(move |this| this.border_color(accent_border))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.emit(HomeEvent::OpenTool(id.clone()));
                }))
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().text_color(accent).child(icon))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(fg)
                                .child(name),
                        ),
                )
                .child(div().text_xs().text_color(muted_fg).child(description))
        });

        // 内容整块窗口内垂直居中（logo + 卡片 < 最小窗高，无需滚动）
        v_flex()
            .size_full()
            .bg(bg)
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(960.0))
                    .p(px(32.0))
                    .gap(px(36.0))
                    .items_center()
                    .child(render_logo(mono, accent, muted_fg))
                    .child(
                        // 原生 flex 行（默认 align 为 stretch）：同排卡片一律等高
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .justify_center()
                            .gap(px(16.0))
                            .children(cards),
                    ),
            )
    }
}

fn render_logo(mono: SharedString, accent: gpui::Hsla, muted_fg: gpui::Hsla) -> impl IntoElement {
    // 顶部稍亮往下逐行掉 alpha 做层次
    let mut lines = Vec::with_capacity(RAMAG_LOGO.len());
    for (i, line) in RAMAG_LOGO.iter().enumerate() {
        let alpha = 1.0 - (i as f32) * 0.06;
        let color = hsla(accent.h, accent.s, accent.l, alpha);
        lines.push(
            div()
                .text_color(color)
                .line_height(px(13.0))
                .child(SharedString::from(line.to_string())),
        );
    }

    v_flex()
        .items_center()
        .gap(px(18.0))
        .child(
            v_flex()
                .font_family(mono.clone())
                .text_size(px(14.0))
                .font_weight(gpui::FontWeight::BOLD)
                .children(lines),
        )
        .child(
            div()
                .font_family(mono)
                .text_size(px(12.0))
                .text_color(muted_fg)
                .child(SharedString::from(
                    "$ minimal by design · local by default_",
                )),
        )
}
