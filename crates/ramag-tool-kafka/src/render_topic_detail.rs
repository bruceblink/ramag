use super::*;

impl KafkaView {
    pub(super) fn render_topic_detail(
        &self,
        topic: &KafkaTopic,
        compact: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let admin_disabled = !self.read_only.allows_admin()
            || self.topic_operation
            || self.loading_runtime
            || self.saving
            || self.deleting;
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
            .when(compact, |panel| panel.w_full().flex_1().min_w_0())
            .when(!compact, |panel| panel.w(px(390.0)).flex_none())
            .h_full()
            .min_h_0()
            .gap(px(8.0))
            .child(section_heading(
                "Topic 详情",
                "Partition、Leader、ISR 与 Offset",
                &theme,
            ))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("kafka-topic-detail-name")
                            .debug_selector(|| "kafka-topic-detail-name".into())
                            .flex_1()
                            .min_w_0()
                            .child(
                                ramag_ui::SelectableText::new(
                                    "kafka-topic-detail-selectable-name",
                                    topic.name.clone(),
                                )
                                .w_full()
                                .text_sm(),
                            ),
                    )
                    .child(
                        ramag_ui::clickable_button("kafka-topic-detail-copy")
                            .debug_selector(|| "kafka-topic-detail-copy".into())
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("复制 Topic 名称")
                            .on_click(cx.listener({
                                let name = topic.name.clone();
                                move |_, _: &ClickEvent, window, cx| {
                                    ramag_ui::copy_text_with_notification(name.clone(), window, cx);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("kafka-partition-scroll")
                            .debug_selector(|| "kafka-partition-scroll".into())
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.topic_partition_scroll)
                            .child(rows),
                    )
                    .child(
                        div()
                            .id("kafka-partition-v-scrollbar")
                            .debug_selector(|| "kafka-partition-v-scrollbar".into())
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(16.0))
                            .bg(theme.scrollbar)
                            .child(
                                Scrollbar::vertical(&self.topic_partition_scroll)
                                    .id("kafka-partition-v-scrollbar-control")
                                    .scrollbar_show(ScrollbarShow::Always),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap(px(8.0))
                    .child(field(
                        "目标 Partition 总数",
                        Input::new(&self.topic_target_partitions).small(),
                        0.0,
                    ))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(8.0))
                            .child(
                                ramag_ui::clickable_button("kafka-topic-expand")
                                    .debug_selector(|| "kafka-topic-expand".into())
                                    .outline()
                                    .small()
                                    .icon(IconName::Plus)
                                    .label("扩容")
                                    .flex_1()
                                    .disabled(admin_disabled || topic.internal)
                                    .tooltip("增加 Partition")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.begin_expand_topic(window, cx);
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("kafka-topic-delete")
                                    .debug_selector(|| "kafka-topic-delete".into())
                                    .danger()
                                    .small()
                                    .icon(IconName::Delete)
                                    .label("删除")
                                    .flex_1()
                                    .disabled(admin_disabled || topic.internal)
                                    .tooltip("删除 Topic")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.begin_delete_topic(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                ramag_ui::clickable_button("kafka-open-topic-messages")
                    .debug_selector(|| "kafka-open-topic-messages".into())
                    .outline()
                    .small()
                    .icon(IconName::Search)
                    .label("浏览消息")
                    .when(compact, |button| button.w_full())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.section = KafkaSection::Messages;
                        cx.notify();
                    })),
            )
    }
}
