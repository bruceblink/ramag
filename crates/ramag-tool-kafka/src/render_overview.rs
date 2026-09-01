use super::*;

impl KafkaView {
    pub(super) fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .id("kafka-welcome")
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(12.0))
            .child(Icon::new(IconName::Network).text_color(theme.accent))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("开始浏览 Kafka"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("新建一个本地集群配置，连接后读取真实的 Broker、Topic 和消息"),
            )
            .child(
                ramag_ui::clickable_button("kafka-welcome-add")
                    .debug_selector(|| "kafka-welcome-add".into())
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("新建集群配置")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.new_profile(window, cx);
                    })),
            )
    }

    pub(super) fn render_workspace(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tabs = KafkaSection::ALL
            .into_iter()
            .fold(h_flex().gap(px(2.0)), |tabs, section| {
                let selected = self.section == section;
                tabs.child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "kafka-section-{:?}",
                        section
                    )))
                    .debug_selector(move || format!("kafka-section-{:?}", section))
                    .small()
                    .label(section.label())
                    .when(selected, |button| button.primary())
                    .when(!selected, |button| button.ghost())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.section = section;
                            if section == KafkaSection::ConsumerGroups
                                && !this.loading_runtime
                                && let Some(config) = this.selected_config()
                            {
                                this.load_consumer_groups(config, window, cx);
                            }
                            if section == KafkaSection::Acls
                                && !this.loading_runtime
                                && !this.acls_loaded
                                && let Some(config) = this.selected_config()
                            {
                                this.load_acls(config, window, cx);
                            }
                            if section == KafkaSection::Config
                                && this.selected_cluster_id.is_some()
                                && this.config_entries.is_empty()
                                && this.config_resource_name.read(cx).value().trim().is_empty()
                            {
                                set_value(
                                    &this.config_resource_name,
                                    this.selected_topic.clone().unwrap_or_default(),
                                    window,
                                    cx,
                                );
                            }
                            cx.notify();
                        },
                    )),
                )
            });
        let panel = match self.section {
            KafkaSection::Overview => self.render_overview(cx).into_any_element(),
            KafkaSection::Topics => self.render_topics(window, cx).into_any_element(),
            KafkaSection::Messages => self.render_messages(window, cx).into_any_element(),
            KafkaSection::ConsumerGroups => {
                self.render_consumer_groups(window, cx).into_any_element()
            }
            KafkaSection::Acls => self.render_acls(window, cx).into_any_element(),
            KafkaSection::Config => self.render_config(window, cx).into_any_element(),
        };
        v_flex()
            .id("kafka-workspace")
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .id("kafka-workspace-tabs")
                    .w_full()
                    .h(px(48.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px(px(22.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(tabs)
                    .when(self.section == KafkaSection::Config, |row| {
                        row.child(
                            h_flex()
                                .gap(px(8.0))
                                .child(
                                    ramag_ui::clickable_button("kafka-save-profile")
                                        .primary()
                                        .small()
                                        .icon(IconName::Check)
                                        .label("保存")
                                        .disabled(
                                            self.saving
                                                || self.testing
                                                || self.deleting
                                                || self.acl_operation,
                                        )
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.save_profile(window, cx);
                                            },
                                        )),
                                )
                                .when(self.selected_cluster_id.is_some(), |row| {
                                    row.child(
                                        ramag_ui::clickable_button("kafka-delete-profile")
                                            .ghost()
                                            .small()
                                            .icon(IconName::Delete)
                                            .tooltip("删除本地配置")
                                            .disabled(
                                                self.saving
                                                    || self.testing
                                                    || self.deleting
                                                    || self.acl_operation,
                                            )
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.delete_profile(window, cx);
                                                },
                                            )),
                                    )
                                }),
                        )
                    }),
            )
            .child(panel)
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let metadata = self.metadata.as_ref();
        let topic_count = self.topics.len();
        let partition_count = self
            .topics
            .iter()
            .map(|topic| topic.partitions.len())
            .sum::<usize>();
        let content = if self.loading_runtime {
            v_flex()
                .id("kafka-overview-scroll")
                .debug_selector(|| "kafka-overview-scroll".into())
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("正在读取 Kafka 元数据…"),
                )
                .into_any_element()
        } else {
            match metadata {
                None => v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .child(Icon::new(IconName::Network).text_color(theme.muted_foreground))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("尚未建立连接"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("请先在配置页保存集群并测试连接"),
                    )
                    .into_any_element(),
                Some(metadata) => v_flex()
                    .id("kafka-overview-scroll")
                    .debug_selector(|| "kafka-overview-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(22.0))
                    .gap(px(18.0))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(12.0))
                            .child(metric_card("Brokers", metadata.brokers.len(), &theme))
                            .child(metric_card("Topics", topic_count, &theme))
                            .child(metric_card("Partitions", partition_count, &theme)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(18.0))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(10.0))
                                    .child(section_heading(
                                        "Broker 元数据",
                                        "来自 Kafka Metadata API",
                                        &theme,
                                    ))
                                    .child(self.render_broker_table(metadata, cx)),
                            )
                            .child(
                                v_flex()
                                    .w(px(320.0))
                                    .flex_none()
                                    .gap(px(10.0))
                                    .child(section_heading(
                                        "集群信息",
                                        "连接状态和协议摘要",
                                        &theme,
                                    ))
                                    .child(self.render_cluster_summary(metadata, cx)),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap(px(10.0))
                            .child(section_heading(
                                "Topic 预览",
                                "仅展示已从 Broker 读取的数据",
                                &theme,
                            ))
                            .child(self.render_topic_preview(cx)),
                    )
                    .into_any_element(),
            }
        };
        div()
            .id("kafka-overview")
            .debug_selector(|| "kafka-overview".into())
            .size_full()
            .child(content)
    }

    pub(super) fn render_broker_table(
        &self,
        metadata: &KafkaClusterMetadata,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut rows = v_flex()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0));
        for broker in metadata.brokers.iter().take(100) {
            rows = rows.child(broker_row(broker, &theme));
        }
        if metadata.brokers.len() > 100 {
            rows = rows.child(
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_xs()
                    .text_color(theme.warning)
                    .child("Broker 数量过多，仅展示前 100 个"),
            );
        }
        rows
    }

    pub(super) fn render_cluster_summary(
        &self,
        metadata: &KafkaClusterMetadata,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .w_full()
            .gap(px(10.0))
            .p(px(14.0))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(summary_row(
                "Cluster ID",
                metadata.cluster_id.as_deref().unwrap_or("未知"),
                &theme,
            ))
            .child(summary_row(
                "Controller",
                &display_option_i32(metadata.controller_id),
                &theme,
            ))
            .child(summary_row(
                "Kafka 版本",
                metadata.kafka_version.as_deref().unwrap_or("未知"),
                &theme,
            ))
            .child(summary_row(
                "读取模式",
                "只读，不提交 Consumer Offset",
                &theme,
            ))
    }

    pub(super) fn render_topic_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.topics.is_empty() {
            return v_flex()
                .w_full()
                .items_center()
                .p(px(18.0))
                .border_1()
                .border_color(theme.border)
                .rounded(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Broker 没有返回 Topic"),
                );
        }
        let mut rows = v_flex()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0));
        for topic in self.topics.iter().take(8) {
            rows = rows.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .px(px(12.0))
                    .py(px(9.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_sm().child(topic.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} partitions", topic.partitions.len())),
                    ),
            );
        }
        rows
    }
}
