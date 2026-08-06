//! JumpServer RDP 收藏与最近会话。

use std::sync::Arc;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, v_flex,
};
use ramag_app::SshService;
use ramag_domain::entities::{JumpServerRdpSession, JumpServerRdpSessionHistory};

use super::SshView;

#[derive(Clone, PartialEq, Eq)]
enum RemoteSessionOperation {
    Opening(String),
    UpdatingFavorite(String),
}

pub(super) struct RemoteSessionPanel {
    service: Arc<SshService>,
    pub(super) history: JumpServerRdpSessionHistory,
    loading: bool,
    operation: Option<RemoteSessionOperation>,
    error: Option<String>,
}

impl RemoteSessionPanel {
    pub(super) fn new(service: Arc<SshService>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            service,
            history: JumpServerRdpSessionHistory::default(),
            loading: true,
            operation: None,
            error: None,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.load_jumpserver_rdp_sessions().await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(history) => {
                        this.history = history;
                        this.error = None;
                    }
                    Err(error) => {
                        this.error = Some(format!("读取远程会话失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_session(&mut self, session: JumpServerRdpSession, cx: &mut Context<Self>) {
        if self.loading || self.operation.is_some() {
            return;
        }
        let identity = session_identity(&session);
        self.operation = Some(RemoteSessionOperation::Opening(identity));
        self.error = None;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = match service
                .create_saved_jumpserver_rdp_web_session(&session)
                .await
            {
                Ok(url) => {
                    let history = service.record_jumpserver_rdp_session(session).await;
                    Ok((url, history))
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                this.operation = None;
                match result {
                    Ok((url, Ok(history))) => {
                        this.history = history;
                        this.error = None;
                        cx.open_url(&url);
                    }
                    Ok((url, Err(error))) => {
                        cx.open_url(&url);
                        this.error = Some(format!(
                            "远程桌面已打开，但更新最近会话失败：{}",
                            error.message()
                        ));
                    }
                    Err(error) => {
                        this.error = Some(format!("打开远程桌面失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_favorite(
        &mut self,
        session: JumpServerRdpSession,
        favorite: bool,
        cx: &mut Context<Self>,
    ) {
        if self.loading || self.operation.is_some() {
            return;
        }
        let identity = session_identity(&session);
        self.operation = Some(RemoteSessionOperation::UpdatingFavorite(identity));
        self.error = None;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service
                .set_jumpserver_rdp_session_favorite(&session, favorite)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.operation = None;
                match result {
                    Ok(history) => {
                        this.history = history;
                        this.error = None;
                    }
                    Err(error) => {
                        let action = if favorite { "收藏" } else { "取消收藏" };
                        this.error = Some(format!("{action}失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_section(
        &self,
        title: &'static str,
        sessions: &[JumpServerRdpSession],
        favorite: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let section_id = if favorite {
            "remote-session-favorites"
        } else {
            "remote-session-recent"
        };
        let empty_message = if favorite {
            "暂无收藏会话"
        } else {
            "从 JumpServer 打开的远程桌面会显示在这里"
        };
        let mut list = v_flex()
            .id(section_id)
            .debug_selector(move || section_id.into())
            .w_full()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .border_1()
            .border_color(border)
            .rounded(px(7.0));
        if sessions.is_empty() {
            list = list.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(14.0))
                    .text_sm()
                    .text_color(muted)
                    .child(empty_message),
            );
        } else {
            for (index, session) in sessions.iter().cloned().enumerate() {
                list = list.child(self.render_session_row(index, session, favorite, cx));
            }
        }
        v_flex()
            .w_full()
            .gap(px(7.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{} 个", sessions.len())),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn render_session_row(
        &self,
        index: usize,
        session: JumpServerRdpSession,
        favorite: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let identity = session_identity(&session);
        let opening = self.operation == Some(RemoteSessionOperation::Opening(identity.clone()));
        let busy = self.loading || self.operation.is_some();
        let session_for_open = session.clone();
        let session_for_favorite = session.clone();
        let account = account_label(&session);
        let endpoint = source_label(&session.jumpserver_url);
        let detail = [
            session.asset_address.as_str(),
            session.asset_platform.as_str(),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        h_flex()
            .id(SharedString::from(format!(
                "remote-session-row-{}-{index}",
                if favorite { "favorite" } else { "recent" }
            )))
            .debug_selector(move || {
                format!(
                    "remote-session-row-{}-{index}",
                    if favorite { "favorite" } else { "recent" }
                )
            })
            .w_full()
            .min_h(px(54.0))
            .items_center()
            .gap(px(10.0))
            .px(px(11.0))
            .py(px(7.0))
            .when(index > 0, |row| row.border_t_1().border_color(border))
            .child(ramag_ui::icons::remote_desktop().small().text_color(muted))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(session.asset_name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(detail),
                    ),
            )
            .child(
                v_flex()
                    .w(px(190.0))
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_xs()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(account),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(endpoint),
                    ),
            )
            .child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "remote-session-open-{favorite}-{index}"
                )))
                .debug_selector(move || format!("remote-session-open-{favorite}-{index}"))
                .outline()
                .small()
                .label(if opening { "打开中…" } else { "打开" })
                .disabled(busy)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.open_session(session_for_open.clone(), cx);
                })),
            )
            .child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "remote-session-favorite-{favorite}-{index}"
                )))
                .debug_selector(move || format!("remote-session-favorite-{favorite}-{index}"))
                .ghost()
                .small()
                .icon(if favorite {
                    IconName::StarFill
                } else {
                    IconName::Star
                })
                .tooltip(if favorite { "取消收藏" } else { "收藏" })
                .disabled(busy)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_favorite(session_for_favorite.clone(), !favorite, cx);
                })),
            )
    }
}

impl Render for RemoteSessionPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = v_flex().w_full().gap(px(16.0));
        if self.loading {
            body = body.child(
                div()
                    .w_full()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("加载中…"),
            );
        } else {
            body = body
                .child(self.render_section("收藏", &self.history.favorites, true, cx))
                .child(self.render_section("最近会话", &self.history.recent, false, cx));
        }
        body.when_some(self.error.clone(), |body, error| {
            body.child(div().text_xs().text_color(cx.theme().danger).child(error))
        })
    }
}

impl SshView {
    pub(super) fn open_remote_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let service = self.service.clone();
        let panel = cx.new(|cx| RemoteSessionPanel::new(service, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let panel = panel.clone();
            dialog
                .title("远程会话")
                .w(px(760.0))
                .pt(px(20.0))
                .px(px(22.0))
                .pb(px(18.0))
                .content(move |content, _, _| content.child(panel.clone()))
        });
    }
}

fn session_identity(session: &JumpServerRdpSession) -> String {
    format!(
        "{}:{}:{}",
        session.connection_id, session.asset_id, session.account_id
    )
}

fn account_label(session: &JumpServerRdpSession) -> String {
    if session.account_username.is_empty() || session.account_username == session.account_name {
        session.account_name.clone()
    } else {
        format!("{} · {}", session.account_name, session.account_username)
    }
}

fn source_label(url: &str) -> String {
    url.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}
