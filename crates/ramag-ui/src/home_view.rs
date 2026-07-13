//! 首页：ANSI Shadow Logo + tagline（工具入口在左侧 ActivityBar）

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, hsla, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _, button::Button, scroll::ScrollableElement as _, v_flex,
};

use ramag_app::{ConnectionService, ToolRegistry};
use ramag_domain::entities::ConnectionId;

/// 首次使用引导的偏好 key（值 "1" = 已看过）
const ONBOARDING_PREF: &str = "onboarding_shown";

#[derive(Debug, Clone)]
pub enum HomeEvent {
    OpenTool(String),
    OpenConnection(ConnectionId),
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
    /// 首次使用引导可见性：启动异步读偏好，未看过则显示；「知道了」后持久化不再出现
    show_onboarding: bool,
    /// 上次停留工具（从 last_tool 偏好异步读回）：解析出 (id, 名称) 展示「继续上次」
    last_tool: Option<(String, String)>,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(
        registry: Arc<ToolRegistry>,
        _service: Arc<ConnectionService>,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Some(storage) = crate::theme::storage_from_cx(cx) {
            let registry_for_last = registry.clone();
            cx.spawn(async move |this, cx| {
                let seen = matches!(
                    storage.get_preference(ONBOARDING_PREF).await,
                    Ok(Some(v)) if v == "1"
                );
                // 读上次停留工具，解析为 (id, 名称) 供「继续上次」按钮
                let last = match storage.get_preference("last_tool").await {
                    Ok(Some(id)) if !id.is_empty() => registry_for_last
                        .find(&id)
                        .map(|t| (id.clone(), t.meta().name.clone())),
                    _ => None,
                };
                let _ = this.update(cx, |this, cx| {
                    if !seen {
                        this.show_onboarding = true;
                    }
                    this.last_tool = last;
                    cx.notify();
                });
            })
            .detach();
        }
        let _ = registry;
        Self {
            show_onboarding: false,
            last_tool: None,
        }
    }

    /// 重新查看快速上手：菜单「帮助 → 重新查看快速上手」触发；再次显示引导卡片。
    /// 不改「已看过」偏好——这是用户主动重看，不应影响下次启动的自动显示逻辑
    pub fn reshow_onboarding(&mut self, cx: &mut Context<Self>) {
        self.show_onboarding = true;
        cx.notify();
    }

    fn dismiss_onboarding(&mut self, cx: &mut Context<Self>) {
        self.show_onboarding = false;
        if let Some(storage) = crate::theme::storage_from_cx(cx) {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = storage.set_preference(ONBOARDING_PREF, "1").await {
                        tracing::warn!(error = %e, "persist onboarding flag failed");
                    }
                })
                .detach();
        }
        cx.notify();
    }
}

impl Render for HomeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let mono = theme.mono_font_family.clone();
        let bg = theme.background;

        v_flex().size_full().bg(bg).overflow_y_scrollbar().child(
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                // 光学居中：底部留白把重心抬到几何中心上方约 120px，避免视觉偏低
                .pb(px(240.0))
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(840.0))
                        .px(px(40.0))
                        .child(render_logo(mono, accent, muted_fg))
                        // 快捷入口：继续上次停留的工具 + 重新查看快速上手
                        .child({
                            let mut row = gpui_component::h_flex()
                                .w_full()
                                .mt(px(24.0))
                                .gap(px(10.0))
                                .items_center()
                                .justify_center();
                            if let Some((id, name)) = self.last_tool.clone() {
                                row = row.child(
                                    Button::new("home-continue")
                                        .small()
                                        .label(format!("继续上次：{name}"))
                                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                            cx.emit(HomeEvent::OpenTool(id.clone()));
                                        })),
                                );
                            }
                            if !self.show_onboarding {
                                row = row.child(
                                    Button::new("home-reshow-onboarding")
                                        .small()
                                        .label("重新查看快速上手")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.reshow_onboarding(cx);
                                        })),
                                );
                            }
                            row
                        })
                        // 首次使用引导（一次性；「知道了」后不再出现）
                        .when(self.show_onboarding, |this| {
                            let border = theme.border;
                            this.child(
                                v_flex()
                                    .w_full()
                                    .mt(px(28.0))
                                    .p(px(14.0))
                                    .gap(px(6.0))
                                    .border_1()
                                    .border_color(border)
                                    .rounded(px(6.0))
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child("快速上手：")
                                    .child("· 左侧图标栏切换工具：DB Client / Git / 剪贴板")
                                    .child("· DB Client 内「数据源管理」新建连接（支持 MySQL / PostgreSQL / Redis / MongoDB）")
                                    .child("· 任意应用内按 ⌘/Ctrl ⇧ V 唤起剪贴板历史抽屉")
                                    .child("· 菜单「帮助 → 快捷键一览」查看全部键位")
                                    .child(
                                        div().pt(px(4.0)).child(
                                            Button::new("onboarding-dismiss")
                                                .small()
                                                .label("知道了")
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _, cx| {
                                                        this.dismiss_onboarding(cx);
                                                    },
                                                )),
                                        ),
                                    ),
                            )
                        }),
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
