use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    v_flex,
};

use super::jumpserver_dialog::{JumpServerOperation, JumpServerPanel};

pub(super) const ASSET_ADDRESS_WIDTH: f32 = 130.0;
pub(super) const ASSET_PLATFORM_WIDTH: f32 = 90.0;
pub(super) const ASSET_ACTION_WIDTH: f32 = 72.0;
pub(super) const ASSET_PANE_HEIGHT: f32 = 520.0;

impl Render for JumpServerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            ramag_ui::push_responsive_notification(window, notification, cx);
        }
        let body_max_h = (window.viewport_size().height * 0.9 - px(100.0)).max(px(360.0));

        div()
            .id("jumpserver-panel-body")
            .w_full()
            .pt(px(2.0))
            .max_h(body_max_h)
            .overflow_y_scroll()
            .child(
                v_flex()
                    .id("jumpserver-asset-table")
                    .debug_selector(|| "jumpserver-asset-table".into())
                    .w_full()
                    .gap(px(16.0))
                    .child(self.render_source_selector(cx))
                    .child(self.render_login_section(cx))
                    .when(self.session.is_some(), |body| {
                        body.child(self.render_asset_section(cx))
                    }),
            )
    }
}

impl JumpServerPanel {
    fn render_login_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("jumpserver-login-section")
            .debug_selector(|| "jumpserver-login-section".into())
            .w_full()
            .gap(px(12.0))
            .child(self.render_connection_selector(cx))
            .when(
                self.selected_connection_id.is_none() || self.editing_connection,
                |section| section.child(self.render_new_connection_form(cx)),
            )
    }

    fn render_new_connection_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_busy();
        let border = cx.theme().border;

        v_flex()
            .id("jumpserver-new-connection-form")
            .debug_selector(|| "jumpserver-new-connection-form".into())
            .w_full()
            .gap(px(10.0))
            .p(px(14.0))
            .border_1()
            .border_color(border)
            .rounded(px(8.0))
            .bg(cx.theme().secondary)
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(if self.editing_connection {
                        "修改 JumpServer 连接"
                    } else {
                        "新建 JumpServer 连接"
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .gap(px(10.0))
                    .child(div().flex_1().min_w_0().child(input_field(
                        "jumpserver-url-field",
                        "地址",
                        &self.base_url,
                        busy,
                    )))
                    .child(div().w(px(106.0)).child(input_field(
                        "jumpserver-ssh-port-field",
                        "SSH 端口",
                        &self.ssh_port,
                        busy,
                    ))),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .gap(px(10.0))
                    .child(div().w(px(240.0)).child(input_field(
                        "jumpserver-username-field",
                        "用户名",
                        &self.username,
                        busy,
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.render_password_field(cx)),
                    ),
            )
            .child(
                h_flex().w_full().items_center().justify_end().child(
                    h_flex()
                        .items_center()
                        .gap(px(8.0))
                        .when(self.editing_connection, |actions| {
                            actions.child(
                                ramag_ui::clickable_button("cancel-edit-jumpserver-connection")
                                    .ghost()
                                    .small()
                                    .label("取消")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.cancel_edit_connection(cx);
                                    })),
                            )
                        })
                        .child(
                            div()
                                .id("test-jumpserver-connection")
                                .debug_selector(|| "test-jumpserver-connection".into())
                                .child(
                                    ramag_ui::clickable_button("test-jumpserver-connection-button")
                                        .outline()
                                        .small()
                                        .label(
                                            if self.operation
                                                == Some(JumpServerOperation::TestingConnection)
                                            {
                                                "测试中…"
                                            } else {
                                                "测试连接"
                                            },
                                        )
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.test_new_connection(cx);
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .id("save-jumpserver-connection")
                                .debug_selector(|| "save-jumpserver-connection".into())
                                .child(
                                    ramag_ui::clickable_button("save-jumpserver-connection-button")
                                        .primary()
                                        .small()
                                        .label(
                                            if self.operation
                                                == Some(JumpServerOperation::SavingConnection)
                                            {
                                                "保存中…"
                                            } else {
                                                "保存"
                                            },
                                        )
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.save_new_connection(cx);
                                        })),
                                ),
                        ),
                ),
            )
    }

    fn render_password_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_busy();
        v_flex()
            .id("jumpserver-password-field")
            .w_full()
            .gap(px(6.0))
            .child(field_label("登录密码"))
            .child(
                div()
                    .id("jumpserver-password-field-input")
                    .debug_selector(|| "jumpserver-password-field-input".into())
                    .w_full()
                    .child(
                        Input::new(&self.password)
                            .suffix(
                                ramag_ui::clickable_button("jumpserver-password-mask")
                                    .ghost()
                                    .xsmall()
                                    .tab_stop(false)
                                    .icon(if self.password_masked {
                                        IconName::Eye
                                    } else {
                                        IconName::EyeOff
                                    })
                                    .tooltip("显示/隐藏")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.toggle_password_mask(window, cx);
                                    })),
                            )
                            .disabled(busy),
                    ),
            )
    }

    fn render_asset_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = self.filtered_assets();
        let visible_count = visible.len();
        let total = self.assets.len();
        let selected_tree_name = self.selected_tree_name();
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let mut asset_list = v_flex()
            .id("jumpserver-asset-list")
            .debug_selector(|| "jumpserver-asset-list".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();

        if visible.is_empty() {
            asset_list = asset_list.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(muted)
                    .child(if total == 0 {
                        "当前用户没有授权资源"
                    } else {
                        "没有匹配的资源"
                    }),
            );
        } else {
            for (index, asset) in visible.into_iter().enumerate() {
                let selected = self.selected_asset_id.as_deref() == Some(asset.id.as_str());
                asset_list = asset_list.child(self.render_asset_row(index, asset.clone(), cx));
                if selected {
                    asset_list =
                        asset_list.child(self.render_selected_asset_detail(index, asset, cx));
                }
            }
        }

        let resources = v_flex()
            .flex_1()
            .min_w_0()
            .h(px(ASSET_PANE_HEIGHT))
            .gap(px(9.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(format!("当前目录：{selected_tree_name}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{visible_count}/{total}")),
                    ),
            )
            .child(
                div()
                    .id("jumpserver-asset-search")
                    .debug_selector(|| "jumpserver-asset-search".into())
                    .w_full()
                    .child(ramag_ui::cleanable_input(
                        &self.search,
                        "clear-jumpserver-asset-search",
                        self.is_busy(),
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(border)
                    .rounded(px(7.0))
                    .overflow_hidden()
                    .child(self.render_asset_header(cx))
                    .child(asset_list),
            );

        v_flex()
            .id("jumpserver-assets-section")
            .debug_selector(|| "jumpserver-assets-section".into())
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .gap(px(12.0))
                    .child(self.render_asset_tree(cx))
                    .child(resources),
            )
    }

    fn render_asset_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(34.0))
            .items_center()
            .px(px(12.0))
            .gap(px(12.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(div().flex_1().min_w_0().child("资源"))
            .child(div().w(px(ASSET_ADDRESS_WIDTH)).child("地址"))
            .child(div().w(px(ASSET_PLATFORM_WIDTH)).child("平台"))
            .child(div().w(px(ASSET_ACTION_WIDTH)).child("操作"))
    }
}

fn field_label(label: &'static str) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label)
}

fn input_field(
    id: &'static str,
    label: &'static str,
    state: &gpui::Entity<InputState>,
    disabled: bool,
) -> impl IntoElement {
    let selector = format!("{id}-input");
    v_flex()
        .id(id)
        .w_full()
        .gap(px(6.0))
        .child(field_label(label))
        .child(
            div()
                .id(SharedString::from(format!("{id}-input")))
                .debug_selector(move || selector)
                .w_full()
                .child(Input::new(state).disabled(disabled)),
        )
}
