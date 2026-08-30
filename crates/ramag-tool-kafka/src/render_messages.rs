use super::*;

impl KafkaView {
    pub(super) fn render_messages(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = self.message_page.as_ref();
        let selected_record = page.and_then(|page| {
            self.selected_message
                .and_then(|index| page.records.get(index))
        });
        let rows = if let Some(page) = page {
            if page.records.is_empty() {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("扫描范围内没有消息"),
                    )
                    .into_any_element()
            } else {
                let records = page.records.clone();
                let header = message_table_header(&theme);
                let body = uniform_list(
                    "kafka-message-list",
                    records.len(),
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|index| {
                                let record = records[index].clone();
                                let selected = this.selected_message == Some(index);
                                this.render_message_row(index, record, selected, cx)
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(header)
                    .child(body)
                    .into_any_element()
            }
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("设置范围后读取消息"),
                )
                .into_any_element()
        };
        let detail = selected_record
            .map(|record| self.render_message_detail(record, cx).into_any_element())
            .unwrap_or_else(|| {
                v_flex()
                    .w(px(360.0))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .px(px(20.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("选择一条消息查看完整预览"),
                    )
                    .into_any_element()
            });
        v_flex()
            .id("kafka-messages")
            .debug_selector(|| "kafka-messages".into())
            .size_full()
            .p(px(18.0))
            .gap(px(12.0))
            .child(self.render_message_controls(window, cx))
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
                            .child(rows),
                    )
                    .child(detail),
            )
    }

    pub(super) fn render_message_controls(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let range_modes = [KafkaRangeMode::Offset, KafkaRangeMode::Time]
            .into_iter()
            .fold(h_flex().gap(px(4.0)), |row, mode| {
                let selected = self.range_mode == mode;
                row.child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "kafka-range-{}",
                        mode.label()
                    )))
                    .small()
                    .label(mode.label())
                    .when(selected, |button| button.primary())
                    .when(!selected, |button| button.ghost())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.range_mode = mode;
                            cx.notify();
                        },
                    )),
                )
            });
        let range_inputs = match self.range_mode {
            KafkaRangeMode::Offset => h_flex()
                .w_full()
                .flex_wrap()
                .gap(px(8.0))
                .child(field(
                    "起始 Offset",
                    Input::new(&self.start_offset_input).small(),
                    130.0,
                ))
                .child(field(
                    "结束 Offset",
                    Input::new(&self.end_offset_input).small(),
                    130.0,
                ))
                .into_any_element(),
            KafkaRangeMode::Time => h_flex()
                .w_full()
                .flex_wrap()
                .gap(px(8.0))
                .child(field(
                    "起始时间",
                    Input::new(&self.start_time_input).small(),
                    230.0,
                ))
                .child(field(
                    "结束时间",
                    Input::new(&self.end_time_input).small(),
                    230.0,
                ))
                .into_any_element(),
        };
        let search_fields = KafkaMessageSearchField::all().into_iter().enumerate().fold(
            h_flex().gap(px(4.0)),
            |row, (index, field)| {
                let selected = self.search_fields[index];
                row.child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "kafka-search-field-{index}"
                    )))
                    .small()
                    .label(search_field_label(field))
                    .when(selected, |button| button.primary())
                    .when(!selected, |button| button.ghost())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.search_fields[index] = !this.search_fields[index];
                            cx.notify();
                        },
                    )),
                )
            },
        );
        v_flex()
            .w_full()
            .flex_none()
            .gap(px(8.0))
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_end()
                    .gap(px(8.0))
                    .child(field("Topic", Input::new(&self.topic_input).small(), 190.0))
                    .child(field(
                        "Partition",
                        Input::new(&self.partition_input).small(),
                        130.0,
                    ))
                    .child(field("范围", range_modes, 0.0))
                    .child(field(
                        "Limit",
                        Input::new(&self.max_records_input).small(),
                        90.0,
                    ))
                    .child(
                        ramag_ui::clickable_button("kafka-read-messages")
                            .debug_selector(|| "kafka-read-messages".into())
                            .primary()
                            .small()
                            .icon(IconName::Search)
                            .label("读取")
                            .disabled(
                                self.loading_runtime
                                    || self.loading_messages
                                    || self.testing
                                    || self.saving,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.read_messages(window, cx);
                            })),
                    )
                    .when(self.loading_messages, |row| {
                        row.child(
                            ramag_ui::clickable_button("kafka-cancel-messages")
                                .debug_selector(|| "kafka-cancel-messages".into())
                                .outline()
                                .small()
                                .icon(IconName::Close)
                                .label("取消")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.cancel_message_read(cx);
                                })),
                        )
                    }),
            )
            .child(range_inputs)
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_end()
                    .gap(px(8.0))
                    .child(
                        div().w(px(260.0)).child(
                            v_flex()
                                .gap(px(5.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("搜索内容（可选）"),
                                )
                                .child(
                                    ramag_ui::cleanable_input(
                                        &self.message_search,
                                        "kafka-message-search-clear",
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
                    .child(field("搜索字段", search_fields, 0.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("只读扫描 · 不提交 Offset"),
                    ),
            )
    }

    pub(super) fn render_message_row(
        &self,
        index: usize,
        record: KafkaMessageRecord,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .id(SharedString::from(format!("kafka-message-row-{index}")))
            .w_full()
            .items_center()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(9.0))
            .border_b_1()
            .border_color(theme.border)
            .when(selected, |row| row.bg(theme.accent.opacity(0.1)))
            .when(!selected, |row| {
                row.hover(|row| row.bg(theme.muted.opacity(0.5)))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected_message = Some(index);
                cx.notify();
            }))
            .child(
                div()
                    .w(px(56.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("P{}", record.partition)),
            )
            .child(
                div()
                    .w(px(90.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(record.offset.to_string()),
            )
            .child(
                div()
                    .w(px(150.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(format_timestamp(record.timestamp)),
            )
            .child(
                div()
                    .w(px(100.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(
                        record
                            .key_preview(96)
                            .map_or_else(|| "<null>".into(), |preview| preview.text),
                    ),
            )
            .child(
                div().flex_1().min_w_0().text_sm().truncate().child(
                    record
                        .value_preview(MESSAGE_PREVIEW_BYTES)
                        .map_or_else(|| "<null>".into(), |preview| preview.text),
                ),
            )
            .child(
                div()
                    .w(px(70.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} headers", record.headers.len())),
            )
    }

    pub(super) fn render_message_detail(
        &self,
        record: &KafkaMessageRecord,
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
            .w(px(360.0))
            .flex_none()
            .min_h_0()
            .gap(px(10.0))
            .child(section_heading(
                "消息详情",
                "UTF-8 可读文本；非 UTF-8 使用转义预览",
                &theme,
            ))
            .child(
                v_flex()
                    .id("kafka-message-detail-scroll")
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
