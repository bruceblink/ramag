//! DbClientView 渲染：顶部连接 Tab Bar + 中心内容（picker / session）

use gpui::{
    AnyView, ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};

use super::{CenterMode, DbClientView};

impl Render for DbClientView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 异步失败提示：Render 持 Window 时统一推送（与各面板同款）
        if let Some(n) = self.pending_notification.take() {
            use gpui_component::WindowExt as _;
            window.push_notification(n, cx);
        }
        // 跨重启恢复：首帧只建占位槽（不连库），仅上次激活的那个立即建会话；
        // 其余标签首次点击时才真正连接，避免恢复 N 个标签时全部拉元数据 / SCAN
        if let Some((configs, active_id)) = self.pending_restore.take() {
            if self.restore_allowed {
                for config in configs {
                    if self.sessions.len() >= super::MAX_CONNECTION_SESSIONS {
                        break;
                    }
                    if self
                        .sessions
                        .iter()
                        .any(|session| session.config.id == config.id)
                    {
                        continue;
                    }
                    self.sessions.push(super::SessionSlot {
                        entity: None,
                        config,
                        stale: false,
                    });
                }
                if !self.sessions.is_empty() {
                    let idx = active_id
                        .and_then(|id| self.sessions.iter().position(|s| s.config.id == id))
                        .unwrap_or(0);
                    self.active_session = Some(idx);
                    self.center = CenterMode::Session;
                    self.materialize_slot(idx, window, cx);
                }
            }
            self.restore_allowed = false;
        }
        // 中央区为激活会话但实体尚未创建（如恢复兜底路径）：此处有 Window，补建
        if matches!(self.center, CenterMode::Session)
            && let Some(idx) = self.active_session
            && self
                .sessions
                .get(idx)
                .is_some_and(|s| s.entity.is_none() && !s.stale)
        {
            self.materialize_slot(idx, window, cx);
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let secondary_bg = theme.secondary;
        let muted_bg = theme.muted;
        let accent = theme.accent;
        let bg = theme.background;
        let warning = theme.warning;

        let active = self.active_session;

        /// Tab 条目的展示快照
        struct TabInfo {
            idx: usize,
            title: String,
            kind_label: &'static str,
            is_active: bool,
            /// 元数据 (loading, has_error)；占位 / stale 槽无实体，恒 (false, false)
            health: (bool, bool),
            stale: bool,
            production: bool,
        }
        let session_titles: Vec<TabInfo> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| TabInfo {
                idx: i,
                title: s.config.name.clone(),
                kind_label: super::driver_kind_label(s.config.driver),
                is_active: Some(i) == active,
                health: s
                    .entity
                    .as_ref()
                    .map(|e| e.health(cx))
                    .unwrap_or((false, false)),
                stale: s.stale,
                production: s.config.production,
            })
            .collect::<Vec<_>>();

        let on_picker_active = matches!(self.center, CenterMode::ConnectionPicker);

        let mut tab_bar = h_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .bg(secondary_bg);

        // 固定 tab：数据源管理
        let picker_btn_active = on_picker_active;
        let mut picker_tab = h_flex()
            .id("picker-tab")
            .items_center()
            .gap_2()
            .px_3()
            .py(px(7.0))
            .border_r_1()
            .border_color(border)
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.show_picker(cx);
            }))
            .child(
                ramag_ui::icons::database()
                    .small()
                    .text_color(if picker_btn_active { fg } else { muted_fg }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if picker_btn_active { fg } else { muted_fg })
                    .child("数据源管理"),
            );

        if picker_btn_active {
            let mut active_bg = accent;
            active_bg.a = 0.15;
            picker_tab = picker_tab.bg(active_bg);
        } else {
            picker_tab = picker_tab.hover(move |this| this.bg(muted_bg));
        }
        tab_bar = tab_bar.child(picker_tab);

        // 右侧 session tabs 横向滚动，不挤压 picker tab
        let mut session_strip = h_flex()
            .id("conn-tabs-scroll")
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&self.sessions_scroll);

        for info in session_titles {
            let TabInfo {
                idx,
                title,
                kind_label,
                is_active,
                health: (h_loading, h_error),
                stale,
                production,
            } = info;
            let tab_id = SharedString::from(format!("conn-tab-{idx}"));
            let close_id = SharedString::from(format!("conn-tab-close-{idx}"));

            // 这里只掌握元数据树状态，不冒充实时连接健康：黄=加载中、红=失败、灰=已加载/未知。
            let dot_color = if h_loading {
                gpui::hsla(45.0 / 360.0, 0.9, 0.55, 1.0)
            } else if h_error {
                gpui::hsla(0.0, 0.7, 0.55, 1.0)
            } else {
                muted_fg
            };
            let metadata_label = if stale {
                Some("需重连")
            } else if h_loading {
                Some("元数据加载中")
            } else if h_error {
                Some("元数据失败")
            } else {
                None
            };
            let metadata_color = if stale { warning } else { dot_color };

            let mut tab = h_flex()
                .id(tab_id)
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py(px(7.0))
                .border_r_1()
                .border_color(border)
                .cursor_pointer()
                .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(dot_color))
                .child(
                    div()
                        .text_xs()
                        .text_color(if is_active { fg } else { muted_fg })
                        .child(title.clone()),
                )
                .child(div().text_xs().text_color(muted_fg).child(kind_label))
                // 生产只读徽标：会话顶部持续可见，与 driver 层拦截、写入口禁用同一语义
                .when(production, |tab| {
                    let mut chip_bg = warning;
                    chip_bg.a = 0.15;
                    tab.child(
                        div()
                            .flex_none()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .bg(chip_bg)
                            .text_xs()
                            .text_color(warning)
                            .child("只读"),
                    )
                })
                .when_some(metadata_label, |tab, label| {
                    tab.child(div().text_xs().text_color(metadata_color).child(label))
                })
                .child(
                    ramag_ui::clickable_button(close_id)
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.close_session(idx, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.select_session(idx, window, cx);
                }));

            if is_active && !on_picker_active {
                let mut active_bg = accent;
                active_bg.a = 0.15;
                tab = tab.bg(active_bg);
            } else {
                tab = tab.hover(move |this| this.bg(muted_bg));
            }

            session_strip = session_strip.child(tab);
        }

        tab_bar = tab_bar.child(session_strip);

        // stale 槽显示"配置已更新"面板（暂停查询与写入，等待用户一键重连）
        let center_view: gpui::AnyElement = match &self.center {
            CenterMode::Session => {
                match active.and_then(|i| self.sessions.get(i).map(|s| (i, s))) {
                    Some((idx, slot)) if slot.stale => self
                        .render_stale_panel(idx, &slot.config.name, cx)
                        .into_any_element(),
                    Some((_, slot)) => match &slot.entity {
                        Some(entity) => {
                            let view: AnyView = entity.to_any_view();
                            div().size_full().child(view).into_any_element()
                        }
                        // 兜底：实体缺失（本帧顶部已尝试补建），显示占位避免空白
                        None => div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(muted_fg)
                            .child("正在打开连接…")
                            .into_any_element(),
                    },
                    None => {
                        let view: AnyView = self.picker.clone().into();
                        div().size_full().child(view).into_any_element()
                    }
                }
            }
            CenterMode::ConnectionPicker => {
                let view: AnyView = self.picker.clone().into();
                div().size_full().child(view).into_any_element()
            }
        };

        v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(tab_bar)
            .child(div().flex_1().min_h_0().child(center_view))
    }
}

impl DbClientView {
    /// 配置已更新的暂停面板：说明原因 + 一键重连 / 关闭标签
    fn render_stale_panel(
        &self,
        idx: usize,
        name: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let warning = theme.warning;

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(warning)
                    .child(format!("连接「{name}」的配置已更新")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child("该标签持有的旧配置已停用（查询与写入已暂停），以避免按旧地址或旧只读设置操作数据库。"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child("手写 SQL / 命令草稿已保留，重新连接后自动恢复。"),
            )
            .child(
                h_flex()
                    .pt_2()
                    .gap_2()
                    .child(
                        ramag_ui::clickable_button("stale-reconnect")
                            .primary()
                            .small()
                            .label("重新连接")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.reconnect_slot(idx, window, cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("stale-close")
                            .ghost()
                            .small()
                            .label("关闭标签")
                            .text_color(fg)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.close_session(idx, cx);
                            })),
                    ),
            )
    }
}
