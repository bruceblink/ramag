//! ConnectionListPanel 渲染：header（搜索 + 新建按钮）+ body（行列表 / 空状态）

use std::ops::Range;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Render, Styled, Window, div, px,
    uniform_list,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};

use super::row::connection_row;
use super::{ConnectionListPanel, ListEvent, syncable_target_ids};

impl Render for ConnectionListPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.loading && self.loaded_revision != self.service.revision() {
            self.refresh(cx);
        }
        // 首次显示即聚焦搜索框，进入页面直接可打字过滤
        if !self.focused_search_once {
            self.focused_search_once = true;
            self.search.update(cx, |state, cx| state.focus(window, cx));
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let accent = theme.accent;
        let border = theme.border;
        let row_hover = theme.muted;
        let bg = theme.background;

        // 行密度按面板宽度分档：固定列在 800px 窗口下已挤占近满，窄窗口隐藏次要列。
        // 面板宽 ≈ 窗口宽 - 左侧活动栏(约 52px)；用窗口宽近似，断点留足余量
        let width = f32::from(window.viewport_size().width);
        let density = if width < 900.0 {
            super::row::RowDensity::Narrow
        } else if width < 1120.0 {
            super::row::RowDensity::Medium
        } else {
            super::row::RowDensity::Full
        };

        let total = self.connections.len();
        let loading = self.loading;
        let visible = self.filtered_indices();
        let visible_count = visible.len();
        let connections = self.connections.clone();
        let syncable_targets = syncable_target_ids(&connections);

        // 大屏限宽 1080px 居中，header 和列表共用同宽容器
        const CONTENT_MAX_W: f32 = 1080.0;

        let header_inner = h_flex()
            .w_full()
            .items_center()
            .gap(px(16.0))
            .child(
                div().flex_1().min_w_0().child(
                    div().max_w(px(360.0)).child(
                        ramag_ui::cleanable_input(
                            &self.search,
                            "connection-search-clear",
                            false,
                            cx,
                        )
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted_fg)),
                    ),
                ),
            )
            .child(
                ramag_ui::clickable_button("add-connection")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|_this, _: &ClickEvent, _, cx| {
                        cx.emit(ListEvent::RequestNew);
                    })),
            );

        let header = h_flex()
            .w_full()
            .justify_center()
            .px(px(24.0))
            .pt(px(22.0))
            .pb(px(16.0))
            .border_b_1()
            .border_color(border)
            .child(div().w_full().max_w(px(CONTENT_MAX_W)).child(header_inner));

        let body: AnyElement = if loading {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(div().text_sm().text_color(muted_fg).child("加载中…"))
                .into_any_element()
        } else if total == 0 {
            // 区分「加载失败」与「真的没有连接」：失败时给错误 + 重试，不误导为空
            if let Some(err) = self.load_error.clone() {
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .child(div().text_sm().text_color(theme.danger).child(err))
                    .child(
                        ramag_ui::clickable_button("conn-list-retry")
                            .small()
                            .label("重试")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.refresh(cx);
                            })),
                    )
                    .into_any_element()
            } else {
                empty_state(cx).into_any_element()
            }
        } else if visible_count == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_sm()
                        .text_color(fg)
                        .child(format!("没有匹配「{}」的连接", self.query)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted_fg)
                        .child("尝试修改关键字或清空搜索"),
                )
                .into_any_element()
        } else {
            let rows = uniform_list(
                "connection-list-rows",
                visible_count,
                cx.processor({
                    let connections = connections.clone();
                    let visible = visible.clone();
                    let syncable_targets = syncable_targets.clone();
                    move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|row_index| {
                                let connection_index = visible[row_index];
                                let conn = connections[connection_index].clone();
                                let is_selected = this.selected.as_ref() == Some(&conn.id);
                                let show_sync = syncable_targets.contains(&conn.id);
                                let version = this.versions.get(&conn.id).cloned();
                                h_flex()
                                    .w_full()
                                    .justify_center()
                                    .px(px(24.0))
                                    .child(div().w_full().max_w(px(CONTENT_MAX_W)).child(
                                        connection_row(
                                            row_index,
                                            conn,
                                            is_selected,
                                            show_sync,
                                            version,
                                            density,
                                            border,
                                            row_hover,
                                            accent,
                                            fg,
                                            muted_fg,
                                            cx,
                                        ),
                                    ))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }
                }),
            )
            .size_full();
            div()
                .size_full()
                .py(px(10.0))
                .child(rows)
                .into_any_element()
        };

        v_flex().size_full().bg(bg).child(header).child(body)
    }
}

/// 空状态：只放一个居中主按钮
fn empty_state(cx: &mut Context<ConnectionListPanel>) -> impl IntoElement {
    v_flex().size_full().items_center().justify_center().child(
        ramag_ui::clickable_button("empty-add")
            .primary()
            .icon(IconName::Plus)
            .label("新建")
            .on_click(cx.listener(|_this, _: &ClickEvent, _, cx| {
                cx.emit(ListEvent::RequestNew);
            })),
    )
}
