//! 首页与工具入口。

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

const RAMAG_LOGO: &[&str] = &[
    "██████╗  █████╗ ███╗   ███╗ █████╗  ██████╗ ",
    "██╔══██╗██╔══██╗████╗ ████║██╔══██╗██╔════╝ ",
    "██████╔╝███████║██╔████╔██║███████║██║  ███╗",
    "██╔══██╗██╔══██║██║╚██╔╝██║██╔══██║██║   ██║",
    "██║  ██║██║  ██║██║ ╚═╝ ██║██║  ██║╚██████╔╝",
    "╚═╝  ╚═╝╚═╝  ╚═╝╚═╝     ╚═╝╚═╝  ╚═╝ ╚═════╝ ",
];
const TOOL_CARD_WIDTH: f32 = 280.0;
const TOOL_CARD_HEIGHT: f32 = 112.0;
const TOOL_CARD_GAP: f32 = 16.0;
const TOOL_GRID_WIDTH: f32 = TOOL_CARD_WIDTH * 3.0 + TOOL_CARD_GAP * 2.0;

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
                .w(px(TOOL_CARD_WIDTH))
                .h(px(TOOL_CARD_HEIGHT))
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
                    .child(render_logo(mono, accent))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(TOOL_GRID_WIDTH))
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .justify_start()
                            .gap(px(TOOL_CARD_GAP))
                            .children(cards),
                    ),
            )
    }
}

fn render_logo(mono: SharedString, accent: gpui::Hsla) -> impl IntoElement {
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
        .font_family(mono)
        .text_size(px(14.0))
        .font_weight(gpui::FontWeight::BOLD)
        .children(lines)
}
