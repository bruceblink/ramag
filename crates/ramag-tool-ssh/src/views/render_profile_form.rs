//! SSH 连接表单；字段密度与数据库连接弹窗保持一致。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    input::Input, v_flex,
};
use ramag_domain::entities::SshAuthMode;

use super::profile_dialog::{FeedbackKind, SshProfileFormPanel};

impl Render for SshProfileFormPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let busy = self.is_busy();
        let default_available = matches!(self.default_capability.as_ref(), Some(Ok(_)));
        let custom_path_entered = !self.form.ssh_path.read(cx).value().trim().is_empty();
        let can_test = default_available || custom_path_entered;
        let viewport_h = window.viewport_size().height;
        let body_max_h = (viewport_h * 0.9 - px(210.0)).max(px(200.0));

        v_flex()
            .w_full()
            .pt(px(4.0))
            .child(
                div()
                    .id("ssh-profile-form-body")
                    .w_full()
                    .max_h(body_max_h)
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .gap(px(16.0))
                            .child(
                                v_flex()
                                    .gap(px(12.0))
                                    .child(field(
                                        "ssh-profile-name-field",
                                        "名称",
                                        Input::new(&self.form.name).disabled(busy),
                                    ))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap(px(12.0))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "ssh-profile-host-field",
                                                "Host",
                                                Input::new(&self.form.host).disabled(busy),
                                            )))
                                            .child(div().w(px(110.0)).child(field(
                                                "ssh-profile-port-field",
                                                "Port",
                                                Input::new(&self.form.port).disabled(busy),
                                            ))),
                                    )
                                    .child(self.render_environment_row(cx))
                                    .child(self.render_production_row(cx)),
                            )
                            .child(self.render_auth_section(cx))
                            .child(self.render_advanced_section(cx)),
                    ),
            )
            .child(div().h(px(1.0)).bg(border).my(px(10.0)))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(12.0))
                            .child(
                                ramag_ui::clickable_button("test-ssh-profile")
                                    .small()
                                    .label("测试")
                                    .disabled(busy || !can_test)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.test_connection(cx);
                                    })),
                            )
                            .when_some(self.feedback.as_ref(), |row, feedback| {
                                let color = match feedback.kind {
                                    FeedbackKind::Info => muted,
                                    FeedbackKind::Success => success,
                                    FeedbackKind::Error => danger,
                                };
                                row.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(color)
                                        .child(feedback.message.clone()),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                ramag_ui::clickable_button("cancel-ssh-profile")
                                    .ghost()
                                    .small()
                                    .label("取消")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.request_cancel(window, cx);
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("save-ssh-profile")
                                    .primary()
                                    .small()
                                    .label("保存")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.save(cx);
                                    })),
                            ),
                    ),
            )
    }
}

impl SshProfileFormPanel {
    fn render_environment_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.form.environment.read(cx).value().trim().to_string();
        let mut row = h_flex().w_full().items_center().gap(px(8.0));
        for preset in ["dev", "test", "prod"] {
            let selected = current == preset;
            row = row.child(
                ramag_ui::clickable_button(SharedString::from(format!("ssh-env-{preset}")))
                    .small()
                    .label(preset)
                    .disabled(self.is_busy())
                    .when(selected, |button| button.primary())
                    .when(!selected, |button| button.ghost())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let value = if this.form.environment.read(cx).value().trim() == preset {
                            ""
                        } else {
                            preset
                        };
                        this.form.environment.update(cx, |state, cx| {
                            state.set_value(value, window, cx);
                        });
                        cx.notify();
                    })),
            );
        }
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .child(Input::new(&self.form.environment).disabled(self.is_busy())),
        );
        v_flex().gap(px(6.0)).child(field_label("环境")).child(row)
    }

    fn render_production_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .id("ssh-production-label")
                    .debug_selector(|| "ssh-production-label".into())
                    .text_sm()
                    .child("生产"),
            )
            .child(
                ramag_ui::clickable_switch("ssh-production")
                    .checked(self.production)
                    .disabled(self.is_busy())
                    .on_click(cx.listener(|this, _: &bool, _, cx| {
                        this.toggle_production(cx);
                    })),
            )
    }

    fn render_auth_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_busy();
        let mode = self.auth_mode;
        v_flex()
            .gap(px(12.0))
            .child(section_title("认证", cx.theme().muted_foreground))
            .child(
                h_flex()
                    .gap(px(8.0))
                    .child(auth_button(
                        "ssh-auth-password",
                        "密码",
                        mode == SshAuthMode::Password,
                        busy,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_auth_mode(SshAuthMode::Password, cx);
                        }),
                    ))
                    .child(auth_button(
                        "ssh-auth-system",
                        "系统",
                        mode == SshAuthMode::System,
                        busy,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_auth_mode(SshAuthMode::System, cx);
                        }),
                    ))
                    .child(auth_button(
                        "ssh-auth-key",
                        "密钥",
                        mode == SshAuthMode::KeyFile,
                        busy,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_auth_mode(SshAuthMode::KeyFile, cx);
                        }),
                    )),
            )
            .child(field(
                "ssh-profile-username-field",
                "用户",
                Input::new(&self.form.username).disabled(busy),
            ))
            .when(mode == SshAuthMode::Password, |section| {
                section.child(field(
                    "ssh-profile-password-field",
                    "密码",
                    Input::new(&self.form.password)
                        .suffix(
                            ramag_ui::clickable_button("ssh-password-mask")
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
                                    this.password_masked = !this.password_masked;
                                    let masked = this.password_masked;
                                    this.form.password.update(cx, |state, cx| {
                                        state.set_masked(masked, window, cx);
                                    });
                                    cx.notify();
                                })),
                        )
                        .disabled(busy),
                ))
            })
            .when(mode == SshAuthMode::KeyFile, |section| {
                section.child(
                    h_flex()
                        .w_full()
                        .items_end()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(field(
                            "ssh-profile-key-field",
                            "密钥",
                            Input::new(&self.form.key_path).disabled(busy),
                        )))
                        .child(
                            ramag_ui::clickable_button("pick-ssh-key")
                                .outline()
                                .small()
                                .label("选择")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.pick_local_path(true, window, cx);
                                })),
                        ),
                )
            })
    }

    fn render_advanced_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_busy();
        v_flex()
            .gap(px(12.0))
            .child(field(
                "ssh-profile-directory-field",
                "目录",
                Input::new(&self.form.initial_directory).disabled(busy),
            ))
            .child(
                v_flex()
                    .id("ssh-profile-executable-field")
                    .debug_selector(|| "ssh-profile-executable-field".into())
                    .w_full()
                    .gap(px(6.0))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap(px(12.0))
                            .child(field_label("SSH 路径"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.render_openssh_status(cx)),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("ssh-profile-executable-field-input")
                                    .debug_selector(|| "ssh-profile-executable-field-input".into())
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.form.ssh_path).disabled(busy)),
                            )
                            .child(
                                ramag_ui::clickable_button("pick-openssh")
                                    .outline()
                                    .small()
                                    .label("选择")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.pick_local_path(false, window, cx);
                                    })),
                            ),
                    ),
            )
    }

    fn render_openssh_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let probing = self.default_capability.is_none();
        let (message, color) = match &self.default_capability {
            None => ("探测中…".to_string(), cx.theme().muted_foreground),
            Some(Ok(capability)) => (
                format!("{} · {}", capability.version, capability.executable),
                cx.theme().success,
            ),
            Some(Err(error)) => (format!("不可用：{error}"), cx.theme().danger),
        };
        h_flex()
            .id("ssh-openssh-status")
            .debug_selector(|| "ssh-openssh-status".into())
            .w_full()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .id("ssh-openssh-label")
                    .debug_selector(|| "ssh-openssh-label".into())
                    .flex_none()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("本机"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(color)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(message),
            )
            .child(
                ramag_ui::clickable_button("retry-openssh-probe")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .disabled(probing || self.is_busy())
                    .tooltip("探测")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.retry_openssh_probe(cx);
                    })),
            )
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

fn field(id: &'static str, label: &'static str, input: Input) -> impl IntoElement {
    let input_selector = format!("{id}-input");
    v_flex()
        .id(id)
        .w_full()
        .gap(px(6.0))
        .child(field_label(label))
        .child(
            div()
                .id(SharedString::from(format!("{id}-input")))
                .debug_selector(move || input_selector)
                .w_full()
                .child(input),
        )
}

fn auth_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    disabled: bool,
    listener: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let selector = id.to_string();
    div().id(id).debug_selector(move || selector.clone()).child(
        ramag_ui::clickable_button(SharedString::from(format!("{id}-button")))
            .small()
            .label(label)
            .disabled(disabled)
            .when(selected, |button| button.primary())
            .when(!selected, |button| button.ghost())
            .on_click(listener),
    )
}
