use super::*;

impl KafkaView {
    pub(super) fn render_message_detail(
        &self,
        record: &KafkaMessageRecord,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let key = record
            .key_preview(MESSAGE_PREVIEW_BYTES)
            .map_or_else(|| "<null>".into(), |preview| preview.text);
        let value = record
            .value_preview(MESSAGE_PREVIEW_BYTES)
            .map_or_else(|| "<null>".into(), |preview| preview.text);
        let record_for_json = record.clone();
        let record_for_hex = record.clone();
        let record_for_base64 = record.clone();
        let record_for_export = record.clone();
        v_flex()
            .id("kafka-message-detail")
            .debug_selector(|| "kafka-message-detail".into())
            .when(compact, |view| view.w_full().flex_1().min_w_0())
            .when(!compact, |view| view.w(px(360.0)).flex_none())
            .min_h_0()
            .gap(px(10.0))
            .child(section_heading(
                "消息详情",
                "UTF-8 保留原文；二进制显示 Hex 摘要",
                &theme,
            ))
            .child(
                v_flex()
                    .id("kafka-message-detail-scroll")
                    .debug_selector(|| "kafka-message-detail-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(px(12.0))
                    .p(px(14.0))
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(6.0))
                    .child(summary_row("Topic", &record.topic, &theme))
                    .child(summary_row(
                        "Partition",
                        &record.partition.to_string(),
                        &theme,
                    ))
                    .child(summary_row("Offset", &record.offset.to_string(), &theme))
                    .child(summary_row(
                        "Timestamp",
                        &format_timestamp(record.timestamp),
                        &theme,
                    ))
                    .child(value_block("Key", key, &theme))
                    .child(value_block("Value", value, &theme))
                    .child(value_block("Headers", format_headers(record), &theme))
                    .child(section_heading(
                        "消息格式",
                        "复制 Value 或导出包含完整原始字节的 JSON",
                        &theme,
                    ))
                    .child(
                        h_flex()
                            .w_full()
                            .flex_wrap()
                            .gap(px(6.0))
                            .child(
                                ramag_ui::clickable_button("kafka-copy-message-json")
                                    .outline()
                                    .xsmall()
                                    .icon(IconName::Copy)
                                    .label("复制 JSON")
                                    .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                                        ramag_ui::copy_text_with_notification(
                                            format_message_json(&record_for_json),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("kafka-copy-message-hex")
                                    .outline()
                                    .xsmall()
                                    .icon(IconName::Copy)
                                    .label("Value Hex")
                                    .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                                        ramag_ui::copy_text_with_notification(
                                            encode_value_hex(&record_for_hex),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("kafka-copy-message-base64")
                                    .outline()
                                    .xsmall()
                                    .icon(IconName::Copy)
                                    .label("Value Base64")
                                    .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                                        ramag_ui::copy_text_with_notification(
                                            encode_value_base64(&record_for_base64),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("kafka-export-message-json")
                                    .primary()
                                    .xsmall()
                                    .icon(IconName::File)
                                    .label("导出 JSON")
                                    .disabled(self.exporting)
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.export_message(
                                                record_for_export.clone(),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                            ),
                    ),
            )
    }
}
