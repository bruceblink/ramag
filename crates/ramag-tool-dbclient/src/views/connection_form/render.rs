//! ConnectionFormPanel Render：driver 选择 + 字段分组 + 测试 / 取消 / 保存

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    v_flex,
};

use super::{ConnectionFormPanel, FormMode, TestState, field_row, section_title};

impl ConnectionFormPanel {
    /// 生产模式开关：track + thumb 拨动样式，开启呈红色警示。
    /// 开启后由 driver 层拦截该连接的一切写 / 改 / 删操作，与颜色标签相互独立
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
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.production = !this.production;
                        cx.notify();
                    }))
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

impl Render for ConnectionFormPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;

        // 失败时须能读全 + 复制诊断，故单列 test_failed 标记：失败文案换行展开、附复制按钮，
        // 成功 / 测试中沿用单行省略
        let (test_msg, test_failed) = match &self.test_state {
            TestState::Idle => (None, false),
            TestState::Testing => (Some(("测试中…".to_string(), muted_fg)), false),
            TestState::Success => (Some(("✓ 连接成功".to_string(), gpui::green())), false),
            TestState::Failed(msg) => (Some((msg.clone(), gpui::red())), true),
        };

        // 内容（不带 dialog 标题/边框，dialog 系统提供）：
        // driver 选择器（仅新建可见）→ 字段分组 → 底部按钮区
        // 注：dialog 自身有 16px padding，这里只补少量上下间距
        let driver_selector: Option<gpui::AnyElement> = matches!(self.mode, FormMode::Create)
            .then(|| self.render_driver_selector(cx).into_any_element());

        // driver 相关的标签 / 占位
        // PG 协议要求连接时必须绑定具体 database，单独标"必填"以区别 MySQL 的可选
        let is_redis = self.driver_id == "redis";
        let database_label = match self.driver_id {
            "redis" => "DB（默认 0-15，自建实例可更高）",
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
            .gap(px(18.0))
            .pt(px(4.0))
            .pb(px(4.0))
            // —— 数据库类型（仅新建时显示，编辑模式 driver 不可变更）——
            .children(driver_selector)
            // —— MongoDB：粘贴连接 URI 一键填充（副本集多主机取首个；+srv 不支持） ——
            .when(self.driver_id == "mongodb", |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .items_end()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(field_row(
                            "从 URI 填充（可选）",
                            Input::new(&self.mongo_uri),
                        )))
                        .child(
                            Button::new("apply-mongo-uri")
                                .small()
                                .label("填充")
                                .tooltip("解析 mongodb:// 地址并回填下方字段")
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.apply_mongo_uri(window, cx);
                                })),
                        ),
                )
            })
            // —— 连接信息 ——
            .child(
                v_flex()
                    .gap(px(12.0))
                    .child(section_title("连接信息", muted_fg))
                    .child(field_row("名称", Input::new(&self.name)))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(field_row("Host", Input::new(&self.host))),
                            )
                            .child(
                                div()
                                    .w(px(110.0))
                                    .child(field_row("Port", Input::new(&self.port))),
                            ),
                    )
                    .child(field_row(database_label, Input::new(&self.database)))
                    .child(self.render_production_toggle(cx)),
            )
            // —— 认证 ——
            .child(
                v_flex()
                    .gap(px(12.0))
                    .child(section_title("认证", muted_fg))
                    .child(field_row(username_label, Input::new(&self.username)))
                    // 密码默认掩码显示，右侧提供显示/隐藏切换按钮
                    .child(field_row("密码", Input::new(&self.password).mask_toggle()))
                    // MongoDB 专属：认证库 authSource（独立于"默认打开的库"）
                    .when(self.driver_id == "mongodb", |this| {
                        this.child(field_row(
                            "认证库 authSource（可选，留空 = admin）",
                            Input::new(&self.auth_source),
                        ))
                    }),
            )
            // —— 传输安全 ——
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
                                gpui_component::switch::Switch::new("conn-tls")
                                    .checked(self.tls)
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
                                Button::new(SharedString::from(format!("tls-verify-{mode:?}")))
                                    .small()
                                    .label(label)
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
                                Input::new(&self.ca_cert_path),
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
                                Input::new(&self.ssh_target),
                            )))
                            .child(
                                div()
                                    .w(px(110.0))
                                    .child(field_row("SSH 端口", Input::new(&self.ssh_port))),
                            ),
                    ),
            )
            // —— 分隔 + 按钮区 ——
            .child(div().h(px(1.0)).bg(border).my(px(2.0)))
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
                            .child(Button::new("test").small().label("测试连接").on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.handle_test(cx);
                                }),
                            ))
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
                                        Button::new("copy-test-err")
                                            .ghost()
                                            .xsmall()
                                            .flex_none()
                                            .label("复制")
                                            .tooltip("复制错误诊断")
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
                                Button::new("cancel")
                                    .ghost()
                                    .small()
                                    .label("取消")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.handle_cancel(cx);
                                    })),
                            )
                            .child(
                                Button::new("save")
                                    .primary()
                                    .small()
                                    .label(if self.saving {
                                        "保存中…"
                                    } else {
                                        "保存"
                                    })
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
