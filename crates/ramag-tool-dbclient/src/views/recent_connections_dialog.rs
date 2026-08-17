//! 数据库会话内的连接快速选择器。

use std::collections::HashSet;

use gpui::{
    AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, ConnectionId, DriverKind};

use super::dbclient_view::DbClientView;

const FAVORITES_PREF: &str = "dbclient_favorite_connections";

pub(super) struct ConnectionPickerPanel {
    service: std::sync::Arc<ConnectionService>,
    owner: Entity<DbClientView>,
    current: Option<ConnectionId>,
    connections: Vec<ConnectionConfig>,
    favorites: HashSet<ConnectionId>,
    search: Entity<InputState>,
    query: String,
    scroll: ScrollHandle,
    loading: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionPickerPanel {
    fn new(
        service: std::sync::Arc<ConnectionService>,
        owner: Entity<DbClientView>,
        current: Option<ConnectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("搜索名称、地址、账号、数据库、类型或环境")
        });
        let subscriptions = vec![cx.observe(&search, |this, _, cx| {
            this.query = this.search.read(cx).value().trim().to_lowercase();
            cx.notify();
        })];
        let mut this = Self {
            service,
            owner,
            current,
            connections: Vec::new(),
            favorites: HashSet::new(),
            search,
            query: String::new(),
            scroll: ScrollHandle::new(),
            loading: true,
            error: None,
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        let storage = ramag_ui::theme::storage_from_cx(cx);
        cx.spawn(async move |this, cx| {
            let connections = service.list().await;
            let favorites = if let Some(storage) = storage {
                match storage.get_preference(FAVORITES_PREF).await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(operation = "dbclient_favorites_load", error = %error, "load favorite connections failed");
                        None
                    }
                }
            } else {
                None
            };
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match connections {
                    Ok(connections) => this.connections = connections,
                    Err(error) => {
                        tracing::error!(
                            operation = "dbclient_connection_list",
                            error = %error,
                            "load connections for picker failed"
                        );
                        this.error = Some(format!("读取连接失败：{error}"));
                    }
                }
                if let Some(json) = favorites {
                    match serde_json::from_str::<Vec<ConnectionId>>(&json) {
                        Ok(ids) => this.favorites = ids.into_iter().collect(),
                        Err(error) => {
                            tracing::warn!(operation = "dbclient_favorites_parse", error = %error, "parse favorite connections failed");
                        }
                    }
                }
                let valid: HashSet<_> = this.connections.iter().map(|item| item.id.clone()).collect();
                this.favorites.retain(|id| valid.contains(id));
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_favorite(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        if !self.favorites.remove(&id) {
            self.favorites.insert(id);
        }
        let mut ids: Vec<_> = self.favorites.iter().cloned().collect();
        ids.sort_by_key(ToString::to_string);
        match serde_json::to_string(&ids) {
            Ok(json) => ramag_ui::preferences::persist_preference_latest(FAVORITES_PREF, json, cx),
            Err(error) => {
                tracing::warn!(operation = "dbclient_favorites_serialize", error = %error, "serialize favorite connections failed");
            }
        }
        cx.notify();
    }

    fn matching_connections(&self) -> Vec<ConnectionConfig> {
        self.connections
            .iter()
            .filter(|connection| connection_matches(connection, &self.query))
            .cloned()
            .collect()
    }

    fn render_section(
        &self,
        title: &'static str,
        connections: Vec<ConnectionConfig>,
        favorite_section: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let count = connections.len();
        let mut body = v_flex()
            .w_full()
            .border_1()
            .border_color(border)
            .rounded(px(8.0))
            .overflow_hidden();
        if connections.is_empty() {
            body = body.child(
                div()
                    .px(px(16.0))
                    .py(px(18.0))
                    .text_sm()
                    .text_color(muted)
                    .child(if favorite_section {
                        "暂无收藏连接"
                    } else if self.query.is_empty() {
                        "暂无连接"
                    } else {
                        "没有匹配的连接"
                    }),
            );
        } else {
            for (index, connection) in connections.into_iter().enumerate() {
                body =
                    body.child(self.render_connection_row(index, connection, favorite_section, cx));
            }
        }
        v_flex()
            .w_full()
            .gap(px(8.0))
            .child(
                h_flex()
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
                            .child(format!("{count} 个")),
                    ),
            )
            .child(body)
    }

    fn render_connection_row(
        &self,
        index: usize,
        connection: ConnectionConfig,
        favorite_section: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let id = connection.id.clone();
        let is_favorite = self.favorites.contains(&id);
        let is_current = self.current.as_ref() == Some(&id);
        let connection_for_open = connection.clone();
        let owner = self.owner.clone();
        let kind = driver_label(connection.driver);
        let endpoint = format!("{}:{}", connection.host, connection.port);
        let account = account_label(&connection);
        let environment = connection.environment.clone().unwrap_or_default();
        let brand_icon = ramag_ui::icons::db_brand_icon(match connection.driver {
            DriverKind::Mysql => "mysql",
            DriverKind::Postgres => "postgres",
            DriverKind::Redis => "redis",
            DriverKind::Mongodb => "mongodb",
        });
        h_flex()
            .id(format!("connection-picker-{favorite_section}-{index}"))
            .w_full()
            .min_h(px(64.0))
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(9.0))
            .when(index > 0, |row| row.border_t_1().border_color(theme.border))
            .child(
                div()
                    .w(px(24.0))
                    .flex()
                    .justify_center()
                    .when_some(brand_icon, |slot, icon| {
                        slot.child(img(icon).size(px(18.0)))
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        h_flex()
                            .gap(px(7.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(connection.name.clone()),
                            )
                            .when(is_current, |row| {
                                row.child(status_badge("当前", theme.accent))
                            })
                            .when(!environment.is_empty(), |row| {
                                row.child(status_badge(environment.clone(), theme.muted_foreground))
                            })
                            .when(connection.production, |row| {
                                row.child(status_badge("生产 · 只读", theme.danger))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(format!("{endpoint} · {kind}")),
                    ),
            )
            .child(
                v_flex()
                    .w(px(210.0))
                    .min_w_0()
                    .gap(px(3.0))
                    .child(div().text_xs().child(account))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(connection.remark.clone().unwrap_or_default()),
                    ),
            )
            .child(
                ramag_ui::clickable_button(format!(
                    "connection-picker-open-{favorite_section}-{index}"
                ))
                .outline()
                .small()
                .label(if is_current { "当前" } else { "打开" })
                .disabled(is_current)
                .on_click(move |_, window, app| {
                    window.close_dialog(app);
                    owner.update(app, |owner, cx| {
                        owner.open_connection_from_picker(connection_for_open.clone(), window, cx)
                    });
                }),
            )
            .child(
                ramag_ui::clickable_button(format!(
                    "connection-picker-favorite-{favorite_section}-{index}"
                ))
                .ghost()
                .small()
                .icon(if is_favorite {
                    IconName::StarFill
                } else {
                    IconName::Star
                })
                .tooltip(if is_favorite {
                    "取消收藏"
                } else {
                    "收藏"
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_favorite(id.clone(), cx);
                })),
            )
    }
}

impl Render for ConnectionPickerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let matching = self.matching_connections();
        let favorites: Vec<_> = matching
            .iter()
            .filter(|connection| self.favorites.contains(&connection.id))
            .cloned()
            .collect();
        let row_count = matching.len().saturating_add(favorites.len()).max(1);
        let desired_height = px((row_count.min(7) as f32 * 64.0) + 160.0);
        let max_height = (window.viewport_size().height * 0.58)
            .max(px(300.0))
            .min(px(590.0));
        let body_height = desired_height.min(max_height).max(px(230.0));
        let all_count = matching.len();
        v_flex()
            .w_full()
            .gap(px(14.0))
            .child(
                h_flex().w_full().child(
                    Input::new(&self.search)
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted)),
                ),
            )
            .child(
                div()
                    .id("connection-picker-scroll")
                    .w_full()
                    .h(body_height)
                    .flex_none()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(if self.loading {
                        v_flex()
                            .h_full()
                            .items_center()
                            .justify_center()
                            .child(div().text_sm().text_color(muted).child("加载中…"))
                    } else {
                        v_flex()
                            .w_full()
                            .gap(px(18.0))
                            .child(self.render_section("收藏", favorites, true, cx))
                            .child(self.render_section("所有连接", matching, false, cx))
                    }),
            )
            .when_some(self.error.clone(), |body, error| {
                body.child(div().text_xs().text_color(danger).child(error))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("共 {all_count} 个匹配连接")),
            )
    }
}

impl DbClientView {
    pub(super) fn open_connection_picker_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .active_session
            .and_then(|index| self.sessions.get(index))
            .map(|slot| slot.config.id.clone());
        let service = self.service.clone();
        let owner = cx.entity().clone();
        let panel = cx.new(|cx| ConnectionPickerPanel::new(service, owner, current, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let panel = panel.clone();
            dialog
                .title("连接选择器")
                .w(px(820.0))
                .margin_top(px(42.0))
                .content(move |content, _, _| content.child(panel.clone()))
        });
    }
}

fn connection_matches(connection: &ConnectionConfig, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let values = [
        connection.name.as_str(),
        connection.host.as_str(),
        connection.username.as_str(),
        connection.database.as_deref().unwrap_or(""),
        connection.environment.as_deref().unwrap_or(""),
        connection.remark.as_deref().unwrap_or(""),
        driver_label(connection.driver),
    ];
    values
        .iter()
        .any(|value| value.to_lowercase().contains(query))
}

fn driver_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    }
}

fn account_label(connection: &ConnectionConfig) -> String {
    let database = connection.database.as_deref().unwrap_or("").trim();
    match (connection.username.trim(), database) {
        ("", "") => "未配置账号".to_string(),
        ("", database) => database.to_string(),
        (username, "") => username.to_string(),
        (username, database) => format!("{username} · {database}"),
    }
}

fn status_badge(label: impl Into<String>, color: gpui::Hsla) -> impl IntoElement {
    let mut background = color;
    background.a = 0.12;
    div()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .text_xs()
        .text_color(color)
        .bg(background)
        .child(label.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_unique_connection_fields() {
        let connection = ConnectionConfig::new_mysql("订单库", "10.0.0.8", 3306, "reader");
        assert!(connection_matches(&connection, "订单"));
        assert!(connection_matches(&connection, "10.0.0.8"));
        assert!(connection_matches(&connection, "mysql"));
        assert!(!connection_matches(&connection, "postgres"));
    }
}
