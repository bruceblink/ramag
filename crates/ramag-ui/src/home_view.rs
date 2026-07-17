//! 首页：ANSI Shadow Logo + tagline（工具入口在左侧 ActivityBar）

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, hsla, prelude::*, px,
};
use gpui_component::{ActiveTheme, Sizable as _, scroll::ScrollableElement as _, v_flex};

use ramag_app::{ConnectionService, ToolRegistry};
use ramag_domain::entities::{ConnectionConfig, DriverKind};

/// 首次使用引导的偏好 key（值 "1" = 已看过）
const ONBOARDING_PREF: &str = "onboarding_shown";
/// 首页只展示少量快捷入口，不长期持有其余连接（其中包含解密后的凭据）。
const HOME_CONNECTION_LIMIT: usize = 8;

#[derive(Debug, Clone)]
pub enum HomeEvent {
    OpenTool(String),
    OpenConnection(Box<ConnectionConfig>),
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
    service: Arc<ConnectionService>,
    /// 首次使用引导可见性：启动异步读偏好，未看过则显示；「知道了」后持久化不再出现
    show_onboarding: bool,
    /// 上次停留工具（从 last_tool 偏好异步读回）：解析出 (id, 名称) 展示「继续上次」
    last_tool: Option<(String, String)>,
    connections: Vec<ConnectionConfig>,
    connections_total: usize,
    connections_loading: bool,
    connections_error: Option<String>,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(
        registry: Arc<ToolRegistry>,
        service: Arc<ConnectionService>,
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
        let service_for_load = service.clone();
        cx.spawn(async move |this, cx| {
            let result = service_for_load.list().await;
            let _ = this.update(cx, |this, cx| {
                this.connections_loading = false;
                match result {
                    Ok(connections) => {
                        (this.connections, this.connections_total) = home_connections(connections);
                        this.connections_error = None;
                    }
                    Err(error) => this.connections_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();

        Self {
            registry,
            service,
            show_onboarding: false,
            last_tool: None,
            connections: Vec::new(),
            connections_total: 0,
            connections_loading: true,
            connections_error: None,
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

    fn refresh_connections(&mut self, cx: &mut Context<Self>) {
        if self.connections_loading {
            return;
        }
        self.connections_loading = true;
        self.connections_error = None;
        cx.notify();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.list().await;
            let _ = this.update(cx, |this, cx| {
                this.connections_loading = false;
                match result {
                    Ok(connections) => {
                        (this.connections, this.connections_total) = home_connections(connections);
                    }
                    Err(error) => this.connections_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
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
        let tool_buttons = self
            .registry
            .list()
            .into_iter()
            .enumerate()
            .map(|(index, tool)| {
                let id = tool.meta().id.clone();
                crate::clickable_button(("home-tool", index))
                    .small()
                    .label(format!("打开 {}", tool.meta().name))
                    .tooltip(tool.meta().description.clone())
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(HomeEvent::OpenTool(id.clone()));
                    }))
            });

        let mut connection_body = v_flex().w_full().gap_2();
        if self.connections_loading {
            connection_body = connection_body.child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child("正在加载已保存连接…"),
            );
        } else if let Some(error) = self.connections_error.clone() {
            connection_body = connection_body
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui::red())
                        .child(format!("连接列表加载失败：{error}")),
                )
                .child(
                    crate::clickable_button("home-connections-retry")
                        .small()
                        .label("重试")
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.refresh_connections(cx)),
                        ),
                );
        } else if self.connections.is_empty() {
            connection_body = connection_body.child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child("尚未保存连接，进入数据库客户端后即可新建。"),
            );
        } else {
            for (index, connection) in self.connections.iter().cloned().enumerate() {
                let label = format_connection(&connection);
                let tooltip = format!(
                    "{}{}",
                    connection
                        .remark
                        .clone()
                        .unwrap_or_else(|| "打开此连接".to_string()),
                    if connection.production {
                        " · 生产只读保护已开启"
                    } else {
                        ""
                    }
                );
                connection_body = connection_body.child(
                    crate::clickable_button(("home-connection", index))
                        .small()
                        .label(label)
                        .tooltip(tooltip)
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            cx.emit(HomeEvent::OpenConnection(Box::new(connection.clone())));
                        })),
                );
            }
            if self.connections_total > self.connections.len() {
                connection_body =
                    connection_body.child(div().text_xs().text_color(muted_fg).child(format!(
                        "另有 {} 个连接，请在数据库客户端中查看",
                        self.connections_total - self.connections.len()
                    )));
            }
        }

        v_flex()
            .size_full()
            .bg(bg)
            .overflow_y_scrollbar()
            .items_center()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(900.0))
                    .p(px(32.0))
                    .gap(px(22.0))
                    .child(render_logo(mono, accent, muted_fg))
                    .child({
                        let mut row = gpui_component::h_flex()
                            .w_full()
                            .gap_2()
                            .flex_wrap()
                            .justify_center();
                        if let Some((id, name)) = self.last_tool.clone() {
                            row = row.child(
                                crate::clickable_button("home-continue")
                                    .small()
                                    .label(format!("继续上次：{name}"))
                                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                        cx.emit(HomeEvent::OpenTool(id.clone()));
                                    })),
                            );
                        }
                        row.children(tool_buttons)
                    })
                    .when(self.show_onboarding, |view| {
                        view.child(
                            v_flex()
                                .w_full()
                                .p(px(14.0))
                                .gap_1()
                                .border_1()
                                .border_color(border)
                                .rounded(px(6.0))
                                .text_sm()
                                .text_color(muted_fg)
                                .child("快速上手")
                                .child("左侧图标栏切换工具；数据库连接在数据库客户端中统一管理。")
                                .child("任意应用内按 ⌘/Ctrl ⇧ V 唤起剪贴板历史抽屉。")
                                .child("菜单「帮助 → 快捷键一览」可查看全部键位。")
                                .child(
                                    crate::clickable_button("onboarding-dismiss")
                                        .small()
                                        .label("知道了")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.dismiss_onboarding(cx)
                                        })),
                                ),
                        )
                    })
                    .child(
                        v_flex()
                            .w_full()
                            .p(px(16.0))
                            .gap_3()
                            .border_1()
                            .border_color(border)
                            .rounded(px(8.0))
                            .child(
                                gpui_component::h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("已保存连接"),
                                    )
                                    .child(
                                        gpui_component::h_flex()
                                            .gap_2()
                                            .child(
                                                crate::clickable_button("home-connections-refresh")
                                                    .small()
                                                    .label("刷新")
                                                    .on_click(cx.listener(
                                                        |this, _: &ClickEvent, _, cx| {
                                                            this.refresh_connections(cx)
                                                        },
                                                    )),
                                            )
                                            .child(
                                                crate::clickable_button("home-connections-manage")
                                                    .small()
                                                    .label("管理连接")
                                                    .on_click(cx.listener(
                                                        |_, _: &ClickEvent, _, cx| {
                                                            cx.emit(HomeEvent::OpenTool(
                                                                "dbclient".to_string(),
                                                            ))
                                                        },
                                                    )),
                                            ),
                                    ),
                            )
                            .child(connection_body),
                    )
                    .when(!self.show_onboarding, |view| {
                        view.child(
                            crate::clickable_button("home-reshow-onboarding")
                                .small()
                                .label("重新查看快速上手")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.reshow_onboarding(cx)
                                })),
                        )
                    }),
            )
    }
}

fn home_connections(mut connections: Vec<ConnectionConfig>) -> (Vec<ConnectionConfig>, usize) {
    let total = connections.len();
    connections.truncate(HOME_CONNECTION_LIMIT);
    connections.shrink_to_fit();
    (connections, total)
}

fn format_connection(connection: &ConnectionConfig) -> String {
    let driver = match connection.driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    };
    let production = if connection.production {
        " · 生产只读"
    } else {
        ""
    };
    format!(
        "{} · {driver} · {}:{}{production}",
        connection.name, connection.host, connection.port
    )
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
