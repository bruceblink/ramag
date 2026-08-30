use super::*;

impl KafkaView {
    pub(super) fn render_topics(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let query = value(&self.topic_search, cx).to_lowercase();
        let visible: Vec<KafkaTopic> = self
            .topics
            .iter()
            .filter(|topic| query.is_empty() || topic.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        let selected_topic = self
            .selected_topic
            .as_ref()
            .and_then(|name| self.topics.iter().find(|topic| &topic.name == name));
        let list = if self.loading_runtime {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("正在读取 Topic…"),
                )
                .into_any_element()
        } else if visible.is_empty() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("没有可显示的 Topic"),
                )
                .into_any_element()
        } else {
            let topics = visible.clone();
            uniform_list(
                "kafka-topic-list",
                topics.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .map(|index| {
                            let topic = topics[index].clone();
                            let selected = this.selected_topic.as_ref() == Some(&topic.name);
                            this.render_topic_row(topic, selected, cx)
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .into_any_element()
        };
        let detail = selected_topic
            .map(|topic| {
                self.render_topic_detail(topic, window, cx)
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                v_flex()
                    .w(px(390.0))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .px(px(22.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("选择 Topic 查看 Partition"),
                    )
                    .into_any_element()
            });
        v_flex()
            .id("kafka-topics")
            .size_full()
            .p(px(18.0))
            .gap(px(12.0))
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Topics"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Topic 与 Partition 元数据来自 Broker"),
                            ),
                    )
                    .child(
                        div().w(px(260.0)).child(
                            ramag_ui::cleanable_input(
                                &self.topic_search,
                                "kafka-topic-search-clear",
                                false,
                                cx,
                            )
                            .small()
                            .prefix(
                                Icon::new(IconName::Search)
                                    .small()
                                    .text_color(theme.muted_foreground),
                            ),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(14.0))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .border_1()
                            .border_color(theme.border)
                            .rounded(px(6.0))
                            .child(list),
                    )
                    .child(detail),
            )
    }

    pub(super) fn render_topic_row(
        &self,
        topic: KafkaTopic,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let name = topic.name.clone();
        h_flex()
            .id(SharedString::from(format!("kafka-topic-row-{name}")))
            .w_full()
            .items_center()
            .justify_between()
            .px(px(14.0))
            .py(px(11.0))
            .border_b_1()
            .border_color(theme.border)
            .when(selected, |row| row.bg(theme.accent.opacity(0.1)))
            .when(!selected, |row| {
                row.hover(|row| row.bg(theme.muted.opacity(0.5)))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_topic(name.clone(), window, cx);
            }))
            .child(
                v_flex()
                    .gap(px(2.0))
                    .child(div().text_sm().child(topic.name))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if topic.internal {
                            "内部 Topic"
                        } else {
                            "用户 Topic"
                        },
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} P", topic.partitions.len())),
            )
    }

    pub(super) fn render_topic_detail(
        &self,
        topic: &KafkaTopic,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut rows = v_flex()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0));
        for partition in topic.partitions.iter().take(MAX_VISIBLE_PARTITIONS) {
            rows = rows.child(partition_row(partition, &theme));
        }
        if topic.partitions.len() > MAX_VISIBLE_PARTITIONS {
            rows = rows.child(
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_xs()
                    .text_color(theme.warning)
                    .child(format!(
                        "Partition 数量过多，仅展示前 {} 个",
                        MAX_VISIBLE_PARTITIONS
                    )),
            );
        }
        v_flex()
            .w(px(390.0))
            .flex_none()
            .min_h_0()
            .gap(px(10.0))
            .child(section_heading(
                &topic.name,
                "Partition、Leader、ISR 与 Offset",
                &theme,
            ))
            .child(
                v_flex()
                    .id("kafka-partition-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(rows),
            )
            .child(
                ramag_ui::clickable_button("kafka-open-topic-messages")
                    .outline()
                    .small()
                    .icon(IconName::Search)
                    .label("浏览消息")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.section = KafkaSection::Messages;
                        cx.notify();
                    })),
            )
    }
}
