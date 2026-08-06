//! JumpServer 资源行与行内账号操作。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    v_flex,
};
use ramag_domain::entities::{JumpServerAccount, JumpServerAsset};

use super::jumpserver_dialog::{JumpServerOperation, JumpServerPanel};
use super::render_jumpserver_dialog::{
    ASSET_ACTION_WIDTH, ASSET_ADDRESS_WIDTH, ASSET_PLATFORM_WIDTH,
};

impl JumpServerPanel {
    pub(super) fn render_asset_row(
        &self,
        index: usize,
        asset: JumpServerAsset,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected_asset_id.as_deref() == Some(asset.id.as_str());
        let active = asset.active;
        let asset_id = asset.id.clone();
        let action_asset_id = asset_id.clone();
        let mut selected_bg = cx.theme().accent;
        selected_bg.a = 0.12;
        let retry = selected && self.detail.is_none() && self.detail_error.is_some();
        let action_label = if !active {
            Some("停用")
        } else if selected && self.operation == Some(JumpServerOperation::LoadingDetail) {
            Some("加载中…")
        } else if retry {
            Some("重试")
        } else {
            None
        };

        h_flex()
            .id(SharedString::from(format!("jumpserver-asset-row-{index}")))
            .debug_selector(move || format!("jumpserver-asset-row-{index}"))
            .w_full()
            .min_h(px(46.0))
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
            .when(!active, |row| row.opacity(0.55))
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
                        format!("{}（已停用）", asset.name)
                    }),
            )
            .child(
                div()
                    .w(px(ASSET_ADDRESS_WIDTH))
                    .text_xs()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(if asset.address.is_empty() {
                        "—".to_string()
                    } else {
                        asset.address
                    }),
            )
            .child(
                div()
                    .w(px(ASSET_PLATFORM_WIDTH))
                    .text_xs()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(if asset.platform.is_empty() {
                        "—".to_string()
                    } else {
                        asset.platform
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!(
                        "jumpserver-asset-action-{index}"
                    )))
                    .debug_selector(move || format!("jumpserver-asset-action-{index}"))
                    .w(px(ASSET_ACTION_WIDTH))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        ramag_ui::clickable_button(SharedString::from(format!(
                            "jumpserver-asset-action-button-{index}"
                        )))
                        .xsmall()
                        .disabled(!active || self.is_busy())
                        .when_some(action_label, |button, label| button.label(label))
                        .when(action_label.is_none(), |button| {
                            button
                                .icon(if selected {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .tooltip(if selected { "收起" } else { "选择账号" })
                        })
                        .when(selected, |button| button.outline())
                        .when(!selected, |button| button.ghost())
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.select_asset(action_asset_id.clone(), cx);
                            },
                        )),
                    ),
            )
    }

    pub(super) fn render_selected_asset_detail(
        &self,
        index: usize,
        asset: JumpServerAsset,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut content = v_flex()
            .id("jumpserver-selected-detail")
            .debug_selector(|| "jumpserver-selected-detail".into())
            .w_full()
            .gap(px(7.0))
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary);

        if self.operation == Some(JumpServerOperation::LoadingDetail) {
            return content
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("正在读取「{}」的授权账号…", asset.name)),
                )
                .into_any_element();
        }

        if let Some(error) = &self.detail_error {
            content = content.child(
                div()
                    .id("jumpserver-detail-error")
                    .debug_selector(|| "jumpserver-detail-error".into())
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }

        let Some(detail) = &self.detail else {
            let asset_id = asset.id.clone();
            return content
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "jumpserver-detail-retry-{index}"
                    )))
                    .outline()
                    .xsmall()
                    .label("重试")
                    .disabled(self.is_busy())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.select_asset(asset_id.clone(), cx);
                        },
                    )),
                )
                .into_any_element();
        };

        let can_operate = detail.ssh_enabled
            && self.selected_account_id.as_ref().is_some_and(|account_id| {
                detail
                    .accounts
                    .iter()
                    .any(|account| &account.id == account_id && account.usable_for_direct_login())
            });
        let can_open_rdp = detail.rdp_web_enabled
            && self.selected_account_id.as_ref().is_some_and(|account_id| {
                detail
                    .accounts
                    .iter()
                    .any(|account| &account.id == account_id && account.usable_for_web_session())
            });
        let mut operation_row = h_flex()
            .w_full()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .w(px(64.0))
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child("授权账号"),
            )
            .child(self.render_account_choices(&detail.accounts, cx));

        if can_open_rdp {
            operation_row = operation_row.child(
                div()
                    .id(SharedString::from(format!("jumpserver-inline-rdp-{index}")))
                    .debug_selector(move || format!("jumpserver-inline-rdp-{index}"))
                    .child(
                        ramag_ui::clickable_button(SharedString::from(format!(
                            "jumpserver-inline-rdp-button-{index}"
                        )))
                        .debug_selector(move || format!("jumpserver-inline-rdp-button-{index}"))
                        .primary()
                        .xsmall()
                        .label(if self.operation == Some(JumpServerOperation::OpeningRdp) {
                            "打开中…"
                        } else {
                            "远程桌面"
                        })
                        .disabled(self.is_busy())
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.open_selected_rdp(cx);
                            },
                        )),
                    ),
            );
        }

        if can_operate {
            let selected_is_saved = self.selected_is_saved();
            operation_row =
                operation_row.child(
                    h_flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "jumpserver-inline-test-{index}"
                                )))
                                .debug_selector(move || format!("jumpserver-inline-test-{index}"))
                                .child(
                                    ramag_ui::clickable_button(SharedString::from(format!(
                                        "jumpserver-inline-test-button-{index}"
                                    )))
                                    .outline()
                                    .xsmall()
                                    .label(
                                        if self.operation == Some(JumpServerOperation::Testing) {
                                            "测试中…"
                                        } else {
                                            "测试"
                                        },
                                    )
                                    .disabled(self.is_busy())
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.test_selected(cx);
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "jumpserver-inline-save-{index}"
                                )))
                                .debug_selector(move || format!("jumpserver-inline-save-{index}"))
                                .child(
                                    ramag_ui::clickable_button(SharedString::from(format!(
                                        "jumpserver-inline-save-button-{index}"
                                    )))
                                    .primary()
                                    .xsmall()
                                    .label(if self.operation == Some(JumpServerOperation::Saving) {
                                        "导入中…"
                                    } else if selected_is_saved {
                                        "已导入"
                                    } else {
                                        "导入"
                                    })
                                    .disabled(self.is_busy() || selected_is_saved)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.save_selected(cx);
                                    })),
                                ),
                        ),
                );
        }

        content.child(operation_row).into_any_element()
    }

    fn render_account_choices(
        &self,
        accounts: &[JumpServerAccount],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if accounts.is_empty() {
            return div()
                .pt(px(5.0))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("无授权账号")
                .into_any_element();
        }
        let mut buttons = h_flex().flex_1().min_w_0().flex_wrap().gap(px(7.0));
        for account in accounts {
            buttons = buttons.child(self.render_account_button(account.clone(), cx));
        }
        buttons.into_any_element()
    }

    fn render_account_button(
        &self,
        account: JumpServerAccount,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected_account_id.as_deref() == Some(account.id.as_str());
        let usable = self.detail.as_ref().is_some_and(|detail| {
            (detail.ssh_enabled && account.usable_for_direct_login())
                || (detail.rdp_web_enabled && account.usable_for_web_session())
        });
        let account_id = account.id.clone();
        let base_label = if account.username.is_empty() || account.username == account.name {
            account.name.clone()
        } else {
            format!("{} · {}", account.name, account.username)
        };
        let label = if usable {
            base_label
        } else if !account.can_connect {
            format!("{base_label}（无连接权限）")
        } else {
            format!("{base_label}（不可直连）")
        };
        ramag_ui::clickable_button(SharedString::from(format!(
            "jumpserver-account-{account_id}"
        )))
        .small()
        .label(label)
        .disabled(self.is_busy() || !usable)
        .tooltip(if usable {
            "选择此账号"
        } else if !account.can_connect {
            "该账号缺少 connect 权限"
        } else if !account.has_secret {
            "该账号未托管密码，无法直接打开远程会话"
        } else {
            "账号名称不符合 SSH 直连要求"
        })
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.outline())
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.select_account(account_id.clone(), cx);
        }))
    }
}
