use super::*;

impl KafkaView {
    pub(super) fn render_config(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sasl = self.security_protocol.uses_sasl();
        let tls = self.security_protocol.uses_tls();
        v_flex()
            .id("kafka-config")
            .size_full()
            .overflow_y_scroll()
            .p(px(22.0))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(900.0))
                    .gap(px(18.0))
                    .child(section_heading("集群配置", "配置保存在本机；认证字段由 Storage 加密保存", &theme))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .child(field("名称", Input::new(&self.name).small(), 0.0))
                            .child(field("Client ID", Input::new(&self.client_id).small(), 0.0)),
                    )
                    .child(field("Bootstrap Servers", Input::new(&self.bootstrap_servers).small(), 0.0))
                    .child(
                        v_flex()
                            .gap(px(8.0))
                            .child(div().text_xs().text_color(theme.muted_foreground).child("安全协议"))
                            .child(self.render_protocols(cx)),
                    )
                    .when(sasl, |form| {
                        form.child(
                            v_flex()
                                .gap(px(10.0))
                                .child(div().text_xs().text_color(theme.muted_foreground).child("SASL 认证"))
                                .child(self.render_sasl_mechanisms(cx))
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap(px(12.0))
                                        .child(field("用户名", Input::new(&self.sasl_username).small(), 0.0))
                                        .child(field("密码", Input::new(&self.sasl_password).small(), 0.0)),
                                ),
                        )
                    })
                    .when(tls, |form| {
                        form.child(
                            v_flex()
                                .gap(px(10.0))
                                .child(div().text_xs().text_color(theme.muted_foreground).child("TLS 证书路径"))
                                .child(field("CA 证书", Input::new(&self.ca_cert_path).small(), 0.0))
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap(px(12.0))
                                        .child(field("客户端证书", Input::new(&self.client_cert_path).small(), 0.0))
                                        .child(field("客户端密钥", Input::new(&self.client_key_path).small(), 0.0)),
                                ),
                        )
                    })
                    .child(field("备注", Input::new(&self.remark).small(), 0.0))
                    .child(
                        h_flex()
                            .w_full()
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
                                    .child("Kafka 页面仅执行元数据和有界消息读取，不生产消息、不提交消费位点。"),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("保存后可在右上角测试连接；未保存的表单也可以直接测试，不会自动写入本机。"),
                    ),
            )
    }

    pub(super) fn render_protocols(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let protocols = [
            (KafkaSecurityProtocol::Plaintext, "PLAINTEXT"),
            (KafkaSecurityProtocol::Ssl, "SSL"),
            (KafkaSecurityProtocol::SaslPlaintext, "SASL_PLAINTEXT"),
            (KafkaSecurityProtocol::SaslSsl, "SASL_SSL"),
        ];
        protocols
            .into_iter()
            .fold(h_flex().gap(px(4.0)), |row, (protocol, label)| {
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
            })
    }

    pub(super) fn render_sasl_mechanisms(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mechanisms = [
            (KafkaSaslMechanism::Plain, "PLAIN"),
            (KafkaSaslMechanism::ScramSha256, "SCRAM-SHA-256"),
            (KafkaSaslMechanism::ScramSha512, "SCRAM-SHA-512"),
        ];
        mechanisms
            .into_iter()
            .fold(h_flex().gap(px(4.0)), |row, (mechanism, label)| {
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
            })
    }
}
