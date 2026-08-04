//! 数据库连接表单。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    input::Input, v_flex,
};

use super::{ConnectionFormPanel, FormMode, TestState, field_row, section_title};

impl ConnectionFormPanel {
    /// 生产模式由驱动层拦截写操作。
    fn render_production_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let muted = theme.muted;
        let on = self.production;
        let danger = gpui::hsla(0.0, 0.7, 0.55, 1.0);

        let track = h_flex()
            .w(px(36.0))
            .h(px(20.0))
            .rounded(px(10.0))
            .bg(if on { danger } else { muted })
            .items_center()
            .px(px(2.0))
            .child(div().size(px(16.0)).rounded_full().bg(gpui::white()));
        let track = if on {
            track.justify_end()
        } else {
            track.justify_start()
        };

        v_flex()
            .gap(px(6.0))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("生产模式（只读保护）"),
            )
            .child(
                h_flex()
                    .id("production-toggle")
                    .items_center()
                    .gap(px(8.0))
                    .when(!self.saving, |row| {
                        row.cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.production = !this.production;
                                cx.notify();
                            }))
                    })
                    .when(self.saving, |row| row.opacity(0.6))
                    .child(track)
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_fg)
                            .child("开启后该连接禁止一切写 / 改 / 删操作"),
                    ),
            )
    }
}

impl ConnectionFormPanel {
    fn render_environment_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.environment.read(cx).value().trim().to_string();
        let mut row = h_flex().w_full().items_center().gap(px(8.0));
        for preset in ["dev", "test", "prod"] {
            let selected = current == preset;
            row = row.child(
                ramag_ui::clickable_button(SharedString::from(format!("conn-env-{preset}")))
                    .small()
                    .label(preset)
                    .disabled(self.saving)
                    .when(selected, |b| b.primary())
                    .when(!selected, |b| b.ghost())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let value = if this.environment.read(cx).value().trim() == preset {
                            ""
                        } else {
                            preset
                        };
                        this.environment.update(cx, |state, cx| {
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
                .child(Input::new(&self.environment).disabled(self.saving)),
        );

        v_flex()
            .gap(px(6.0))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("环境标签（可选，列表中显示为徽章）"),
            )
            .child(row)
    }
}

impl Render for ConnectionFormPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        // 小窗口仅滚动主体，保证底部操作可见。
        let viewport_h = window.viewport_size().height;
        let body_max_h = (viewport_h * 0.9 - px(210.0)).max(px(200.0));

        // 失败诊断完整展示，其他状态保持单行。
        let (test_msg, test_failed) = match &self.test_state {
            TestState::Idle => (None, false),
            TestState::Testing => (Some(("测试中…".to_string(), muted_fg)), false),
            TestState::Success => (Some(("✓ 连接成功".to_string(), gpui::green())), false),
            TestState::Failed(msg) => (Some((msg.clone(), gpui::red())), true),
        };

        let driver_selector: Option<gpui::AnyElement> = matches!(self.mode, FormMode::Create)
            .then(|| self.render_driver_selector(cx).into_any_element());

        let is_redis = self.driver_id == "redis";
        let database_label = match self.driver_id {
            "redis" => "DB（默认 0-15）",
            "postgres" => "默认库（必填）",
            "mongodb" => "默认打开的库（可选）",
            _ => "默认库（可选）",
        };
        let username_label = if is_redis {
            "用户名（ACL，可选）"
        } else {
            "用户名"
        };

        v_flex()
            .w_full()
            .pt(px(4.0))
            .child(
                // max_h 与滚动放在同一元素上，溢出量即可滚动量
                // （不渲染滚动条：滚轮 / 触控板直接滚，避免常显轨道贯穿表单）
                div()
                    .id("conn-form-body")
                    .w_full()
                    .max_h(body_max_h)
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .gap(px(18.0))
            .children(driver_selector)
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().child(field_row(
                        "连接 URI（编辑回填不含密码）",
                        Input::new(&self.uri).disabled(self.saving),
                    )))
                    .child(
                        ramag_ui::clickable_button("apply-uri")
                            .small()
                            .label("填充")
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.apply_uri(window, cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(12.0))
                    .child(section_title("连接信息", muted_fg))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .child(div().flex_1().min_w_0().child(field_row(
                                "Host",
                                Input::new(&self.host).disabled(self.saving),
                            )))
                            .child(div().w(px(110.0)).child(field_row(
                                "Port",
                                Input::new(&self.port).disabled(self.saving),
                            ))),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .child(div().flex_1().min_w_0().child(field_row(
                                "名称",
                                Input::new(&self.name).disabled(self.saving),
                            )))
                            .child(div().flex_1().min_w_0().child(field_row(
                                database_label,
                                Input::new(&self.database).disabled(self.saving),
                            ))),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(12.0))
                    .child(section_title("认证", muted_fg))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .child(div().flex_1().min_w_0().child(field_row(
                                username_label,
                                Input::new(&self.username).disabled(self.saving),
                            )))
                            .child(div().flex_1().min_w_0().child(field_row(
                                "密码",
                                Input::new(&self.password)
                                    .suffix(
                                        ramag_ui::clickable_button("password-mask-toggle")
                                            .ghost()
                                            .xsmall()
                                            .tab_stop(false)
                                            .icon(if self.password_masked {
                                                IconName::Eye
                                            } else {
                                                IconName::EyeOff
                                            })
                                            .disabled(self.saving)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    if this.saving {
                                                        return;
                                                    }
                                                    this.password_masked = !this.password_masked;
                                                    let password_masked = this.password_masked;
                                                    this.password.update(cx, |state, cx| {
                                                        state.set_masked(
                                                            password_masked,
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                    cx.notify();
                                                },
                                            )),
                                    )
                                    .disabled(self.saving),
                            )))
                            .when(self.driver_id == "mongodb", |row| {
                                row.child(div().flex_1().min_w_0().child(field_row(
                                    "authSource（留空 = admin）",
                                    Input::new(&self.auth_source).disabled(self.saving),
                                )))
                            }),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(12.0))
                    .child(section_title("标签与保护", muted_fg))
                    .child(self.render_environment_row(cx))
                    .child(self.render_production_toggle(cx)),
            )
            .child(
                v_flex()
                    .gap(px(12.0))
                    .child(section_title("传输安全", muted_fg))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .child(
                                v_flex()
                                    .gap(px(2.0))
                                    .child(div().text_sm().child("启用 TLS 加密"))
                                    .child(div().text_xs().text_color(muted_fg).child(
                                        "关闭时按服务端能力协商（SQL 有则用）；开启则强制加密连接",
                                    )),
                            )
                            .child(
                                ramag_ui::clickable_switch("conn-tls")
                                    .checked(self.tls)
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|this, _: &bool, _, cx| {
                                        this.tls = !this.tls;
                                        this.invalidate_test(cx);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(self.tls, |this| {
                        // 身份验证三档：加密 ≠ 确认对端身份，明示各档语义
                        let current = self.tls_verify;
                        let mut verify_row = h_flex().w_full().items_center().gap(px(8.0));
                        for (mode, label) in [
                            (
                                ramag_domain::entities::TlsVerify::Full,
                                "验证证书与主机名（推荐）",
                            ),
                            (ramag_domain::entities::TlsVerify::Ca, "仅验证 CA 证书链"),
                            (
                                ramag_domain::entities::TlsVerify::None,
                                "仅加密（不验证身份）",
                            ),
                        ] {
                            let selected = current == mode;
                            verify_row = verify_row.child(
                                ramag_ui::clickable_button(SharedString::from(format!("tls-verify-{mode:?}")))
                                    .small()
                                    .label(label)
                                    .disabled(self.saving)
                                    .when(selected, |b| b.primary())
                                    .when(!selected, |b| b.ghost())
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        if this.tls_verify != mode {
                                            this.tls_verify = mode;
                                            this.invalidate_test(cx);
                                            cx.notify();
                                        }
                                    })),
                            );
                        }
                        this.child(verify_row)
                            .child(field_row(
                                "自定义 CA 证书（PEM，自签服务端用；留空用系统信任链验证）",
                                Input::new(&self.ca_cert_path).disabled(self.saving),
                            ))
                            // Redis / Mongo 驱动无「验链不验名」档：Ca 档在这两类上等同严格校验
                            .when(
                                matches!(self.driver_id, "redis" | "mongodb")
                                    && current == ramag_domain::entities::TlsVerify::Ca,
                                |t| {
                                    t.child(div().text_xs().text_color(muted_fg).child(
                                        "该数据库驱动不支持「验链不验名」，此档等同完整验证",
                                    ))
                                },
                            )
                    })
                    // SSH 与 TLS 同开时验证等级受限（隧道内主机名校验必败），如实提示
                    .when(self.tls, |t| {
                        let ssh_on = !self
                            .ssh_target
                            .read(cx)
                            .value()
                            .trim()
                            .is_empty();
                        t.when(ssh_on, |t| {
                            t.child(div().text_xs().text_color(muted_fg).child(
                                "经 SSH 隧道时身份验证自动降级：MySQL/PG 最高验 CA，Redis/Mongo 仅加密（隧道段已由 SSH 加密）",
                            ))
                        })
                    })
                    // SSH 跳板：target 非空即启用；密钥 / agent / 别名由系统 ssh 处理，
                    // 不在此存任何 SSH 凭证
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .child(div().flex_1().min_w_0().child(field_row(
                                "SSH 跳板（可选，需已配密钥或 ssh-agent）",
                                Input::new(&self.ssh_target).disabled(self.saving),
                            )))
                            .child(
                                div()
                                    .w(px(110.0))
                                    .child(field_row(
                                        "SSH 端口",
                                        Input::new(&self.ssh_port).disabled(self.saving),
                                    )),
                            ),
                    ),
            )
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
                                ramag_ui::clickable_button("test")
                                    .small()
                                    .label(if matches!(self.test_state, TestState::Testing) {
                                        "测试中…"
                                    } else {
                                        "测试连接"
                                    })
                                    .disabled(
                                        self.saving
                                            || matches!(self.test_state, TestState::Testing),
                                    )
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.handle_test(cx);
                                    })),
                            )
                            .when_some(test_msg, |this, (msg, color)| {
                                let msg_for_copy = msg.clone();
                                let msg_el = div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::NORMAL)
                                    .text_color(color)
                                    // 失败诊断换行全量展示，成功 / 测试中保持单行省略
                                    .when(!test_failed, |d| d.overflow_hidden().text_ellipsis())
                                    .child(msg);
                                if test_failed {
                                    this.child(msg_el).child(
                                        ramag_ui::clickable_button("copy-test-err")
                                            .ghost()
                                            .xsmall()
                                            .flex_none()
                                            .label("复制")
                                            .on_click(cx.listener(
                                                move |_, _: &ClickEvent, _, cx| {
                                                    cx.write_to_clipboard(
                                                        gpui::ClipboardItem::new_string(
                                                            msg_for_copy.clone(),
                                                        ),
                                                    );
                                                },
                                            )),
                                    )
                                } else {
                                    this.child(msg_el)
                                }
                            }),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.0))
                            .flex_none()
                            .child(
                                ramag_ui::clickable_button("cancel")
                                    .ghost()
                                    .small()
                                    .label("取消")
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.handle_cancel(window, cx);
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("save")
                                    .primary()
                                    .small()
                                    .label(if self.saving {
                                        "保存中…"
                                    } else {
                                        "保存"
                                    })
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        if !this.saving {
                                            this.handle_save(cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}
