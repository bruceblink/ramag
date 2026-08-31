use super::*;

impl KafkaView {
    pub(super) fn render_config(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sasl = self.security_protocol.uses_sasl();
        let tls = self.security_protocol.uses_tls();
        let compact = f32::from(window.viewport_size().width) < 1040.0;
        v_flex()
            .id("kafka-config")
            .debug_selector(|| "kafka-config".into())
            .size_full()
            .overflow_y_scroll()
            .p(px(22.0))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .max_w(px(1280.0))
                    .gap(px(18.0))
                    .child(section_heading("集群配置", "配置保存在本机；认证字段由 Storage 加密保存", &theme))
                    .child(
                        h_flex()
                            .id("kafka-admin-mode-panel")
                            .debug_selector(|| "kafka-admin-mode-panel".into())
                            .w_full()
                            .items_center()
                            .justify_between()
                            .when(compact, |panel| panel.flex_col().items_start())
                            .gap(px(12.0))
                            .px(px(12.0))
                            .py(px(10.0))
                            .border_1()
                            .border_color(if self.read_only.allows_admin() {
                                theme.warning.opacity(0.45)
                            } else {
                                theme.border
                            })
                            .rounded(px(6.0))
                            .child(
                                v_flex()
                                    .id("kafka-admin-mode-copy")
                                    .debug_selector(|| "kafka-admin-mode-copy".into())
                                    .flex_1()
                                    .min_w_0()
                                    .when(compact, |copy| {
                                        copy.flex_initial().w_full()
                                    })
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Topic 管理模式"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(if self.read_only.allows_admin() {
                                                "已允许创建、删除和增加 Partition；每次操作仍需确认"
                                            } else {
                                                "默认只读；关闭后不会调用 Kafka Admin API"
                                            }),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(px(8.0))
                                    .when(compact, |row| row.w_full().justify_between())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if self.read_only.allows_admin() {
                                                theme.warning
                                            } else {
                                                theme.muted_foreground
                                            })
                                            .child(if self.read_only.allows_admin() {
                                                "管理已启用"
                                            } else {
                                                "只读保护"
                                            }),
                                    )
                                    .child(
                                        ramag_ui::clickable_switch("kafka-admin-mode")
                                            .checked(self.read_only.allows_admin())
                                            .on_click(cx.listener(|this, _: &bool, _, cx| {
                                                this.toggle_admin_mode(cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .when(compact, |row| row.flex_col().items_stretch())
                            .child(
                                field("名称", Input::new(&self.name).small(), 0.0)
                                    .when(!compact, |field| field.flex_1().min_w_0()),
                            )
                            .child(
                                field("Client ID", Input::new(&self.client_id).small(), 0.0)
                                    .when(!compact, |field| field.flex_1().min_w_0()),
                            ),
                    )
                    .child(field("Bootstrap Servers", Input::new(&self.bootstrap_servers).small(), 0.0))
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap(px(8.0))
                            .child(div().text_xs().text_color(theme.muted_foreground).child("安全协议"))
                            .child(self.render_protocols(cx)),
                    )
                    .when(sasl, |form| {
                        form.child(
                            v_flex()
                                .w_full()
                                .min_w_0()
                                .gap(px(10.0))
                                .child(div().text_xs().text_color(theme.muted_foreground).child("SASL 认证"))
                                .child(self.render_sasl_mechanisms(cx))
                                .child(
                                    h_flex()
                                    .w_full()
                                        .gap(px(12.0))
                                        .when(compact, |row| row.flex_col().items_stretch())
                                        .child(flexible_field(
                                            "用户名",
                                            Input::new(&self.sasl_username).small(),
                                        ).when(compact, |field| field.flex_initial().w_full()))
                                        .child(flexible_field(
                                            "密码",
                                            Input::new(&self.sasl_password).small(),
                                        ).when(compact, |field| field.flex_initial().w_full())),
                                ),
                        )
                    })
                    .when(tls, |form| {
                        form.child(
                            v_flex()
                                .w_full()
                                .min_w_0()
                                .gap(px(10.0))
                                .child(div().text_xs().text_color(theme.muted_foreground).child("TLS 证书路径"))
                                .child(field("CA 证书", Input::new(&self.ca_cert_path).small(), 0.0))
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap(px(12.0))
                                        .when(compact, |row| row.flex_col().items_stretch())
                                        .child(flexible_field(
                                            "客户端证书",
                                            Input::new(&self.client_cert_path).small(),
                                        ).when(compact, |field| field.flex_initial().w_full()))
                                        .child(flexible_field(
                                            "客户端密钥",
                                            Input::new(&self.client_key_path).small(),
                                        ).when(compact, |field| field.flex_initial().w_full())),
                                ),
                        )
                    })
                    .child(field("备注", Input::new(&self.remark).small(), 0.0))
                    .child(
                        h_flex()
                            .w_full()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .px(px(12.0))
                            .py(px(10.0))
                            .bg(theme.accent.opacity(0.08))
                            .rounded(px(6.0))
                            .child(Icon::new(IconName::CircleCheck).text_color(theme.accent))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Kafka 消息浏览不会生产消息或提交消费位点；Topic 管理写操作需要单独确认。"),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("保存后可在右上角测试连接；未保存的表单也可以直接测试，不会自动写入本机。"),
                    ),
            )
            .child(self.render_remote_config(window, cx))
    }

    pub(super) fn render_protocols(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let protocols = [
            (KafkaSecurityProtocol::Plaintext, "PLAINTEXT"),
            (KafkaSecurityProtocol::Ssl, "SSL"),
            (KafkaSecurityProtocol::SaslPlaintext, "SASL_PLAINTEXT"),
            (KafkaSecurityProtocol::SaslSsl, "SASL_SSL"),
        ];
        protocols.into_iter().fold(
            h_flex().flex_wrap().gap(px(4.0)),
            |row, (protocol, label)| {
                let selected = self.security_protocol == protocol;
                row.child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "kafka-protocol-{label}"
                    )))
                    .small()
                    .label(label)
                    .when(selected, |button| button.primary())
                    .when(!selected, |button| button.ghost())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.security_protocol = protocol;
                            cx.notify();
                        },
                    )),
                )
            },
        )
    }

    pub(super) fn render_sasl_mechanisms(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mechanisms = [
            (KafkaSaslMechanism::Plain, "PLAIN"),
            (KafkaSaslMechanism::ScramSha256, "SCRAM-SHA-256"),
            (KafkaSaslMechanism::ScramSha512, "SCRAM-SHA-512"),
        ];
        mechanisms.into_iter().fold(
            h_flex().flex_wrap().gap(px(4.0)),
            |row, (mechanism, label)| {
                let selected = self.sasl_mechanism == mechanism;
                row.child(
                    ramag_ui::clickable_button(SharedString::from(format!("kafka-sasl-{label}")))
                        .small()
                        .label(label)
                        .when(selected, |button| button.primary())
                        .when(!selected, |button| button.ghost())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.sasl_mechanism = mechanism;
                            cx.notify();
                        })),
                )
            },
        )
    }
}
