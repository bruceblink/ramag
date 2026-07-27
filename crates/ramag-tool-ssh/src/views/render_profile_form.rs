//! SSH 连接表单渲染；尺寸与数据库连接弹窗保持一致。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex, input::Input,
    v_flex,
};
use ramag_domain::entities::SshAuthMode;

use super::profile_dialog::{FeedbackKind, FormOperation, SshProfileFormPanel};

impl Render for SshProfileFormPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let muted_bg = cx.theme().muted;
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
                            .gap(px(18.0))
                            .child(
                                v_flex()
                                    .gap(px(12.0))
                                    .child(section_title("连接信息", muted))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap(px(12.0))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "ssh-profile-name-field",
                                                "名称",
                                                Input::new(&self.form.name).disabled(busy),
                                            )))
                                            .child(div().w(px(150.0)).child(field(
                                                "ssh-profile-color-field",
                                                "颜色标签（#RRGGBB）",
                                                Input::new(&self.form.color).disabled(busy),
                                            ))),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap(px(12.0))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "ssh-profile-host-field",
                                                "主机 / SSH config 别名",
                                                Input::new(&self.form.host).disabled(busy),
                                            )))
                                            .child(div().w(px(110.0)).child(field(
                                                "ssh-profile-port-field",
                                                "端口",
                                                Input::new(&self.form.port).disabled(busy),
                                            ))),
                                    ),
                            )
                            .child(self.render_auth_section(cx))
                            .child(
                                v_flex()
                                    .gap(px(12.0))
                                    .child(section_title("启动选项", muted))
                                    .child(field(
                                        "ssh-profile-directory-field",
                                        "初始远程目录（可选）",
                                        Input::new(&self.form.initial_directory).disabled(busy),
                                    ))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_end()
                                            .gap(px(8.0))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "ssh-profile-executable-field",
                                                "自定义 OpenSSH 可执行文件（可选，必须为绝对路径）",
                                                Input::new(&self.form.ssh_path).disabled(busy),
                                            )))
                                            .child(
                                                ramag_ui::clickable_button("pick-openssh")
                                                    .outline()
                                                    .small()
                                                    .label("选择…")
                                                    .disabled(busy)
                                                    .on_click(cx.listener(
                                                        |this, _: &ClickEvent, window, cx| {
                                                            this.pick_local_path(false, window, cx);
                                                        },
                                                    )),
                                            ),
                                    )
                                    .child(self.render_openssh_status(cx)),
                            )
                            .child(
                                div()
                                    .rounded(px(6.0))
                                    .bg(muted_bg)
                                    .p(px(12.0))
                                    .text_xs()
                                    .text_color(muted)
                                    .child(
                                        "Terminal 可交互输入密码、密钥口令并确认首次主机指纹；SFTP 仅支持 SSH config / Agent 或无需交互的 OpenSSH 密钥，不支持密码、.ppk 或 Pageant。",
                                    ),
                            ),
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
                                    .label(if self.operation == Some(FormOperation::Testing) {
                                        "测试中…"
                                    } else {
                                        "测试连接"
                                    })
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
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.request_cancel(window, cx);
                                        },
                                    )),
                            )
                            .child(
                                ramag_ui::clickable_button("save-ssh-profile")
                                    .primary()
                                    .small()
                                    .label(if self.operation == Some(FormOperation::Saving) {
                                        "保存中…"
                                    } else {
                                        "保存"
                                    })
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
    fn render_auth_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_busy();
        let system = self.auth_mode == SshAuthMode::System;
        v_flex()
            .gap(px(12.0))
            .child(section_title("认证", cx.theme().muted_foreground))
            .child(field(
                "ssh-profile-username-field",
                "用户名（可选，留空交给 OpenSSH 配置）",
                Input::new(&self.form.username).disabled(busy),
            ))
            .child(
                v_flex()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("认证方式"),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(auth_button(
                                "ssh-auth-system",
                                "系统 SSH 配置 / Agent",
                                system,
                                busy,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.set_auth_mode(SshAuthMode::System, cx);
                                }),
                            ))
                            .child(auth_button(
                                "ssh-auth-key",
                                "指定 OpenSSH 密钥",
                                !system,
                                busy,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.set_auth_mode(SshAuthMode::KeyFile, cx);
                                }),
                            )),
                    ),
            )
            .when(!system, |section| {
                section.child(
                    h_flex()
                        .w_full()
                        .items_end()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(field(
                            "ssh-profile-key-field",
                            "密钥文件绝对路径（只保存路径，不读取私钥内容）",
                            Input::new(&self.form.key_path).disabled(busy),
                        )))
                        .child(
                            ramag_ui::clickable_button("pick-ssh-key")
                                .outline()
                                .small()
                                .label("选择…")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.pick_local_path(true, window, cx);
                                })),
                        ),
                )
            })
    }

    fn render_openssh_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let probing = self.default_capability.is_none();
        let (message, color) = match &self.default_capability {
            None => (
                "正在探测系统 OpenSSH…".to_string(),
                cx.theme().muted_foreground,
            ),
            Some(Ok(capability)) => (
                format!(
                    "系统 OpenSSH：{}（{}）",
                    capability.version, capability.executable
                ),
                cx.theme().success,
            ),
            Some(Err(error)) => (format!("OpenSSH 不可用：{error}"), cx.theme().danger),
        };
        h_flex()
            .w_full()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(color)
                    .child(message),
            )
            .child(
                ramag_ui::clickable_button("retry-openssh-probe")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .label(if probing {
                        "探测中…"
                    } else {
                        "重新探测"
                    })
                    .disabled(probing || self.is_busy())
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

fn field(id: &'static str, label: &'static str, input: Input) -> impl IntoElement {
    let input_selector = format!("{id}-input");
    v_flex()
        .id(id)
        .w_full()
        .gap(px(6.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label),
        )
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
    ramag_ui::clickable_button(id)
        .small()
        .label(label)
        .disabled(disabled)
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.ghost())
        .on_click(listener)
}
