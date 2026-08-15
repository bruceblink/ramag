//! 各工具共用的最近项目选择器。

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    v_flex,
};

#[derive(Clone)]
pub struct RecentItem {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub secondary: String,
    pub badge: Option<String>,
    pub current: bool,
    pub icon: IconName,
}

impl RecentItem {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        detail: impl Into<String>,
        icon: IconName,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            detail: detail.into(),
            secondary: String::new(),
            badge: None,
            current: false,
            icon,
        }
    }

    pub fn secondary(mut self, value: impl Into<String>) -> Self {
        self.secondary = value.into();
        self
    }

    pub fn badge(mut self, value: impl Into<String>) -> Self {
        self.badge = Some(value.into());
        self
    }

    pub fn current(mut self, value: bool) -> Self {
        self.current = value;
        self
    }
}

type SelectHandler = Arc<dyn Fn(String, &mut Window, &mut App)>;

struct RecentItemPicker {
    items: Vec<RecentItem>,
    favorites: HashSet<String>,
    preference_key: &'static str,
    search: Entity<InputState>,
    query: String,
    focused_search: bool,
    scroll: ScrollHandle,
    on_select: SelectHandler,
    _subscriptions: Vec<Subscription>,
}

impl RecentItemPicker {
    fn new(
        items: Vec<RecentItem>,
        preference_key: &'static str,
        placeholder: String,
        on_select: SelectHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| crate::bounded_search_input(window, cx).placeholder(placeholder));
        let subscriptions = vec![cx.observe(&search, |this, _, cx| {
            this.query = this.search.read(cx).value().trim().to_lowercase();
            cx.notify();
        })];
        let mut this = Self {
            items,
            favorites: HashSet::new(),
            preference_key,
            search,
            query: String::new(),
            focused_search: false,
            scroll: ScrollHandle::new(),
            on_select,
            _subscriptions: subscriptions,
        };
        this.load_favorites(cx);
        this
    }

    fn load_favorites(&mut self, cx: &mut Context<Self>) {
        let Some(storage) = crate::theme::storage_from_cx(cx) else {
            return;
        };
        let preference_key = self.preference_key;
        cx.spawn(async move |this, cx| {
            let value = storage.get_preference(preference_key).await;
            let _ = this.update(cx, |this, cx| {
                match value {
                    Ok(Some(json)) => match serde_json::from_str::<Vec<String>>(&json) {
                        Ok(ids) => {
                            let valid: HashSet<_> =
                                this.items.iter().map(|item| item.id.as_str()).collect();
                            this.favorites = ids
                                .into_iter()
                                .filter(|id| valid.contains(id.as_str()))
                                .collect();
                        }
                        Err(error) => tracing::warn!(
                            operation = "recent_items_favorites_parse",
                            error = %error,
                            preference = preference_key,
                            "parse recent item favorites failed"
                        ),
                    },
                    Ok(None) => {}
                    Err(error) => tracing::warn!(
                        operation = "recent_items_favorites_load",
                        error = %error,
                        preference = preference_key,
                        "load recent item favorites failed"
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_favorite(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.favorites.remove(&id) {
            self.favorites.insert(id);
        }
        let mut ids: Vec<_> = self.favorites.iter().cloned().collect();
        ids.sort();
        match serde_json::to_string(&ids) {
            Ok(json) => {
                crate::preferences::persist_preference_latest(self.preference_key, json, cx)
            }
            Err(error) => tracing::warn!(
                operation = "recent_items_favorites_serialize",
                error = %error,
                "serialize recent item favorites failed"
            ),
        }
        cx.notify();
    }

    fn matching_items(&self) -> Vec<RecentItem> {
        self.items
            .iter()
            .filter(|item| item_matches(item, &self.query))
            .cloned()
            .collect()
    }

    fn render_section(
        &self,
        title: &'static str,
        items: Vec<RecentItem>,
        favorite_section: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let mut rows = v_flex()
            .w_full()
            .border_1()
            .border_color(border)
            .rounded(px(8.0))
            .overflow_hidden();
        if items.is_empty() {
            rows = rows.child(
                div()
                    .px(px(16.0))
                    .py(px(17.0))
                    .text_sm()
                    .text_color(muted)
                    .child(if favorite_section {
                        "暂无收藏"
                    } else if self.query.is_empty() {
                        "暂无项目"
                    } else {
                        "没有匹配的项目"
                    }),
            );
        } else {
            for (index, item) in items.iter().cloned().enumerate() {
                rows = rows.child(self.render_row(index, item, favorite_section, cx));
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
                            .child(format!("{} 个", items.len())),
                    ),
            )
            .child(rows)
    }

    fn render_row(
        &self,
        index: usize,
        item: RecentItem,
        favorite_section: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let favorite = self.favorites.contains(&item.id);
        let id_for_open = item.id.clone();
        let id_for_favorite = item.id.clone();
        let on_select = self.on_select.clone();
        h_flex()
            .id(format!("recent-picker-row-{favorite_section}-{index}"))
            .w_full()
            .min_h(px(66.0))
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(9.0))
            .when(index > 0, |row| row.border_t_1().border_color(theme.border))
            .child(
                Icon::new(item.icon)
                    .size(px(18.0))
                    .text_color(theme.muted_foreground),
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
                                    .child(item.name),
                            )
                            .when(item.current, |row| row.child(badge("当前", theme.accent)))
                            .when_some(item.badge, |row, value| {
                                row.child(badge(value, theme.muted_foreground))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(item.detail),
                    ),
            )
            .when(!item.secondary.is_empty(), |row| {
                row.child(
                    div()
                        .w(px(210.0))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(item.secondary),
                )
            })
            .child(
                crate::clickable_button(format!("recent-picker-open-{favorite_section}-{index}"))
                    .outline()
                    .small()
                    .label(if item.current { "当前" } else { "打开" })
                    .disabled(item.current)
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        on_select(id_for_open.clone(), window, cx);
                    }),
            )
            .child(
                crate::clickable_button(format!(
                    "recent-picker-favorite-{favorite_section}-{index}"
                ))
                .ghost()
                .small()
                .icon(if favorite {
                    IconName::StarFill
                } else {
                    IconName::Star
                })
                .tooltip(if favorite { "取消收藏" } else { "收藏" })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_favorite(id_for_favorite.clone(), cx);
                })),
            )
    }
}

impl Render for RecentItemPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_search {
            self.focused_search = true;
            self.search
                .update(cx, |search, cx| search.focus(window, cx));
        }
        let matching = self.matching_items();
        let favorites: Vec<_> = matching
            .iter()
            .filter(|item| self.favorites.contains(&item.id))
            .cloned()
            .collect();
        let row_count = matching.len().saturating_add(favorites.len()).max(1);
        let desired_height = px((row_count.min(7) as f32 * 66.0) + 160.0);
        let max_height = (window.viewport_size().height * 0.58)
            .max(px(260.0))
            .min(px(590.0));
        let body_height = desired_height.min(max_height).max(px(230.0));
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap(px(14.0))
            .child(
                Input::new(&self.search)
                    .small()
                    .prefix(Icon::new(IconName::Search).small().text_color(muted)),
            )
            .child(
                div()
                    .id("recent-picker-scroll")
                    .w_full()
                    .h(body_height)
                    .flex_none()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        v_flex()
                            .w_full()
                            .gap(px(18.0))
                            .child(self.render_section("收藏", favorites, true, cx))
                            .child(self.render_section("所有项目", matching, false, cx)),
                    ),
            )
    }
}

pub fn open_recent_item_picker(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<String>,
    search_placeholder: impl Into<String>,
    preference_key: &'static str,
    items: Vec<RecentItem>,
    on_select: SelectHandler,
) {
    let panel = cx.new(|cx| {
        RecentItemPicker::new(
            items,
            preference_key,
            search_placeholder.into(),
            on_select,
            window,
            cx,
        )
    });
    let title = title.into();
    window.open_dialog(cx, move |dialog, _, _| {
        let panel = panel.clone();
        dialog
            .title(title.clone())
            .w(px(820.0))
            .margin_top(px(42.0))
            .content(move |content, _, _| content.child(panel.clone()))
    });
}

fn item_matches(item: &RecentItem, query: &str) -> bool {
    query.is_empty()
        || [&item.name, &item.detail, &item.secondary]
            .iter()
            .any(|value| value.to_lowercase().contains(query))
        || item
            .badge
            .as_ref()
            .is_some_and(|value| value.to_lowercase().contains(query))
}

fn badge(label: impl Into<String>, color: gpui::Hsla) -> impl IntoElement {
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
    fn search_checks_name_details_and_badge() {
        let item = RecentItem::new("1", "生产仓库", "/srv/app", IconName::Folder)
            .secondary("Git")
            .badge("只读");
        assert!(item_matches(&item, "生产"));
        assert!(item_matches(&item, "/srv"));
        assert!(item_matches(&item, "git"));
        assert!(item_matches(&item, "只读"));
        assert!(!item_matches(&item, "mysql"));
    }
}
