//! JumpServer 资源弹窗渲染。

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
use ramag_domain::entities::{JumpServerAccount, JumpServerAsset};

use super::jumpserver_dialog::{JumpServerFeedbackKind, JumpServerOperation, JumpServerPanel};

impl Render for JumpServerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let busy = self.is_busy();
        let can_operate = self.detail.as_ref().is_some_and(|detail| {
            detail.ssh_enabled
                && self.selected_account_id.as_ref().is_some_and(|account_id| {
                    detail.accounts.iter().any(|account| {
                        &account.id == account_id && account.usable_for_direct_login()
                    })
                })
        });
        let selected_is_saved = self.selected_is_saved();
        let body_max_h = (window.viewport_size().height * 0.9 - px(180.0)).max(px(320.0));

        v_flex()
            .w_full()
            .pt(px(4.0))
            .child(
                div()
                    .id("jumpserver-panel-body")
                    .w_full()
                    .max_h(body_max_h)
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .gap(px(16.0))
                            .child(self.render_login_section(cx))
                            .when(self.session.is_some(), |body| {
                                body.child(self.render_asset_section(cx))
                            }),
                    ),
            )
            .child(div().h(px(1.0)).bg(border).my(px(10.0)))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().flex_1().min_w_0().when_some(
                        self.feedback.as_ref(),
                        |content, feedback| {
                            let color = match feedback.kind {
                                JumpServerFeedbackKind::Info => muted,
                                JumpServerFeedbackKind::Success => success,
                                JumpServerFeedbackKind::Error => danger,
                            };
                            content.child(
                                div()
                                    .id("jumpserver-feedback")
                                    .debug_selector(|| "jumpserver-feedback".into())
                                    .text_xs()
                                    .text_color(color)
                                    .whitespace_normal()
                                    .child(feedback.message.clone()),
                            )
                        },
                    ))
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                ramag_ui::clickable_button("close-jumpserver")
                                    .ghost()
                                    .small()
                                    .label("关闭")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.request_cancel(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id("test-jumpserver-asset")
                                    .debug_selector(|| "test-jumpserver-asset".into())
                                    .child(
                                        ramag_ui::clickable_button("test-jumpserver-asset-button")
                                            .outline()
                                            .small()
                                            .label(
                                                if self.operation
                                                    == Some(JumpServerOperation::Testing)
                                                {
                                                    "测试中…"
                                                } else {
                                                    "测试"
                                                },
                                            )
                                            .disabled(busy || !can_operate)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    this.test_selected(cx);
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .id("save-jumpserver-asset")
                                    .debug_selector(|| "save-jumpserver-asset".into())
                                    .child(
                                        ramag_ui::clickable_button("save-jumpserver-asset-button")
                                            .primary()
                                            .small()
                                            .label(
                                                if self.operation
                                                    == Some(JumpServerOperation::Saving)
                                                {
                                                    "保存中…"
                                                } else if selected_is_saved {
                                                    "已保存"
                                                } else {
                                                    "保存"
                                                },
                                            )
                                            .disabled(busy || !can_operate || selected_is_saved)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    this.save_selected(cx);
                                                },
                                            )),
                                    ),
                            ),
                    ),
            )
    }
}

impl JumpServerPanel {
    fn render_login_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_busy();
        v_flex()
            .id("jumpserver-login-section")
            .debug_selector(|| "jumpserver-login-section".into())
            .w_full()
            .gap(px(12.0))
            .child(section_title(
                "JumpServer 登录",
                cx.theme().muted_foreground,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("登录信息仅保留在当前弹窗；保存时只写入选中资源的 SSH 配置。"),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .gap(px(12.0))
                    .child(div().flex_1().min_w_0().child(input_field(
                        "jumpserver-url-field",
                        "地址",
                        &self.base_url,
                        busy,
                    )))
                    .child(div().w(px(110.0)).child(input_field(
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
                    .gap(px(12.0))
                    .child(div().flex_1().min_w_0().child(input_field(
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
                    )
                    .child(
                        div()
                            .id("load-jumpserver-assets")
                            .debug_selector(|| "load-jumpserver-assets".into())
                            .child(
                                ramag_ui::clickable_button("load-jumpserver-assets-button")
                                    .primary()
                                    .small()
                                    .label(
                                        if self.operation
                                            == Some(JumpServerOperation::LoadingAssets)
                                        {
                                            "获取中…"
                                        } else if self.session.is_some() {
                                            "刷新资源"
                                        } else {
                                            "获取资源"
                                        },
                                    )
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.load_assets(cx);
                                    })),
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
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let mut asset_list = div()
            .id("jumpserver-asset-list")
            .debug_selector(|| "jumpserver-asset-list".into())
            .w_full()
            .h(px(260.0))
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
                        "暂无授权资源"
                    } else {
                        "暂无匹配"
                    }),
            );
        } else {
            for (index, asset) in visible.into_iter().enumerate() {
                asset_list = asset_list.child(self.render_asset_row(index, asset, cx));
            }
        }

        v_flex()
            .id("jumpserver-assets-section")
            .debug_selector(|| "jumpserver-assets-section".into())
            .w_full()
            .gap(px(10.0))
            .child(section_title("授权资源", muted))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .id("jumpserver-asset-search")
                            .debug_selector(|| "jumpserver-asset-search".into())
                            .flex_1()
                            .min_w_0()
                            .child(ramag_ui::cleanable_input(
                                &self.search,
                                "clear-jumpserver-asset-search",
                                self.is_busy(),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{visible_count}/{total}")),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .border_1()
                    .border_color(border)
                    .rounded(px(6.0))
                    .overflow_hidden()
                    .child(self.render_asset_header(cx))
                    .child(asset_list),
            )
            .when_some(self.detail.as_ref(), |section, _| {
                section.child(self.render_account_section(cx))
            })
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
            .child(div().flex_1().min_w_0().child("名称"))
            .child(div().w(px(180.0)).child("地址"))
            .child(div().w(px(120.0)).child("平台"))
    }

    fn render_asset_row(
        &self,
        index: usize,
        asset: JumpServerAsset,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected_asset_id.as_deref() == Some(asset.id.as_str());
        let active = asset.active;
        let asset_id = asset.id.clone();
        let mut selected_bg = cx.theme().accent;
        selected_bg.a = 0.14;
        h_flex()
            .id(SharedString::from(format!("jumpserver-asset-row-{index}")))
            .debug_selector(move || format!("jumpserver-asset-row-{index}"))
            .w_full()
            .h(px(38.0))
            .items_center()
            .px(px(12.0))
            .gap(px(12.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .when(selected, |row| row.bg(selected_bg))
            .when(!selected && active, |row| {
                row.hover(|row| row.bg(cx.theme().muted))
            })
            .when(active && !self.is_busy(), |row| {
                row.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_asset(asset_id.clone(), cx);
                    }))
            })
            .when(!active, |row| row.opacity(0.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(if active {
                        asset.name
                    } else {
                        format!("{}（停用）", asset.name)
                    }),
            )
            .child(
                div()
                    .w(px(180.0))
                    .text_xs()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(asset.address),
            )
            .child(
                div()
                    .w(px(120.0))
                    .text_xs()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(asset.platform),
            )
    }

    fn render_account_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.detail.as_ref();
        let accounts = detail
            .map(|detail| detail.accounts.clone())
            .unwrap_or_default();
        let content = v_flex()
            .id("jumpserver-account-section")
            .debug_selector(|| "jumpserver-account-section".into())
            .w_full()
            .gap(px(8.0))
            .child(section_title("资产账号", cx.theme().muted_foreground));
        if accounts.is_empty() {
            return content
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("没有授权账号"),
                )
                .into_any_element();
        }
        let mut buttons = h_flex().w_full().flex_wrap().gap(px(8.0));
        for account in accounts {
            buttons = buttons.child(self.render_account_button(account, cx));
        }
        content.child(buttons).into_any_element()
    }

    fn render_account_button(
        &self,
        account: JumpServerAccount,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected_account_id.as_deref() == Some(account.id.as_str());
        let usable = account.usable_for_direct_login();
        let account_id = account.id.clone();
        let label = if account.username.is_empty() || account.username == account.name {
            account.name.clone()
        } else {
            format!("{} · {}", account.name, account.username)
        };
        ramag_ui::clickable_button(SharedString::from(format!(
            "jumpserver-account-{account_id}"
        )))
        .small()
        .label(label)
        .disabled(self.is_busy() || !usable)
        .tooltip(if usable {
            "选择资产账号"
        } else {
            "缺少连接权限或托管凭据"
        })
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.outline())
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.select_account(account_id.clone(), cx);
        }))
    }
}

fn section_title(text: &str, muted: gpui::Hsla) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(text.to_string()),
        )
        .child(div().flex_1().h(px(1.0)).bg(muted).opacity(0.12))
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
