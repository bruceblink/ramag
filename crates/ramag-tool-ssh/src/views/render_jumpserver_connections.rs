//! JumpServer 导入来源与已保存连接选择。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, img,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    v_flex,
};

use super::jumpserver_dialog::JumpServerPanel;

impl JumpServerPanel {
    pub(super) fn render_source_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = cx.theme().accent;
        let mut tint = accent;
        tint.a = 0.10;
        let mut border = accent;
        border.a = 0.55;

        v_flex()
            .id("jumpserver-source-section")
            .debug_selector(|| "jumpserver-source-section".into())
            .w_full()
            .gap(px(8.0))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("连接来源"),
            )
            .child(
                h_flex()
                    .id("jumpserver-source-selector")
                    .debug_selector(|| "jumpserver-source-selector".into())
                    .w_full()
                    .items_center()
                    .child(
                        h_flex()
                            .w(px(190.0))
                            .items_center()
                            .justify_center()
                            .gap(px(7.0))
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .bg(tint)
                            .text_color(accent)
                            .child(
                                img(ramag_ui::icons::jumpserver_brand_icon())
                                    .size(px(20.0))
                                    .flex_none(),
                            )
                            .child(div().text_sm().child("JumpServer")),
                    ),
            )
    }

    pub(super) fn render_connection_selector(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let accent = cx.theme().accent;
        let mut selected_background = accent;
        selected_background.a = 0.10;
        let busy = self.is_busy();

        let mut selector = v_flex().w_full().gap(px(8.0)).child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(muted)
                                .child("已保存的连接"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{} 个", self.connections.len())),
                        ),
                )
                .child(
                    div()
                        .id("new-jumpserver-connection")
                        .debug_selector(|| "new-jumpserver-connection".into())
                        .child(
                            ramag_ui::clickable_button("new-jumpserver-connection-button")
                                .small()
                                .icon(IconName::Plus)
                                .label("新建连接")
                                .disabled(busy)
                                .when(self.selected_connection_id.is_none(), |button| {
                                    button.primary()
                                })
                                .when(self.selected_connection_id.is_some(), |button| {
                                    button.outline()
                                })
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.new_connection(window, cx);
                                })),
                        ),
                ),
        );

        if self.connections.is_empty() {
            selector = selector.child(
                div()
                    .w_full()
                    .p(px(12.0))
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .text_xs()
                    .text_color(muted)
                    .child("暂无已保存连接，请新建连接。"),
            );
        } else {
            let mut list = v_flex()
                .id("jumpserver-saved-connections")
                .debug_selector(|| "jumpserver-saved-connections".into())
                .w_full()
                .max_h(px(152.0))
                .overflow_y_scroll()
                .border_1()
                .border_color(border)
                .rounded_md()
                .overflow_hidden();

            for (index, connection) in self.connections.iter().enumerate() {
                let selected = self.selected_connection_id.as_deref() == Some(&connection.id);
                let connection_id = connection.id.clone();
                let edit_connection_id = connection.id.clone();
                let delete_connection_id = connection.id.clone();
                let label = connection_label(&connection.credential);
                let row = h_flex()
                    .id(SharedString::from(format!(
                        "jumpserver-connection-{connection_id}"
                    )))
                    .w_full()
                    .h(px(38.0))
                    .items_center()
                    .gap(px(8.0))
                    .px(px(11.0))
                    .when(index > 0, |row| row.border_t_1().border_color(border))
                    .when(selected, |row| {
                        row.bg(selected_background).text_color(accent)
                    })
                    .child(
                        img(ramag_ui::icons::jumpserver_brand_icon())
                            .size(px(17.0))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(label),
                    )
                    .when(selected, |row| {
                        row.child(
                            h_flex()
                                .items_center()
                                .gap(px(2.0))
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .child(
                                    ramag_ui::clickable_button("edit-jumpserver-connection")
                                        .ghost()
                                        .xsmall()
                                        .icon(ramag_ui::icons::pencil())
                                        .tooltip("修改连接")
                                        .disabled(busy)
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                this.edit_connection(
                                                    edit_connection_id.clone(),
                                                    window,
                                                    cx,
                                                );
                                            },
                                        )),
                                )
                                .child(
                                    ramag_ui::clickable_button("delete-jumpserver-connection")
                                        .ghost()
                                        .xsmall()
                                        .icon(ramag_ui::icons::trash())
                                        .tooltip("删除连接")
                                        .disabled(busy)
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                this.request_delete_connection(
                                                    delete_connection_id.clone(),
                                                    window,
                                                    cx,
                                                );
                                            },
                                        )),
                                ),
                        )
                    })
                    .when(!busy, |row| {
                        row.cursor_pointer()
                            .hover(|style| style.bg(cx.theme().muted))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.select_connection(connection_id.clone(), window, cx);
                            }))
                    });
                list = list.child(row);
            }
            selector = selector.child(list);
        }

        selector.into_any_element()
    }
}

fn connection_label(credential: &ramag_domain::entities::JumpServerCredential) -> String {
    let endpoint = credential
        .base_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    format!("{} @ {endpoint}", credential.username)
}
