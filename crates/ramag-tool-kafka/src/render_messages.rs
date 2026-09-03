use super::*;
use ramag_ui::RestrictScrollToAxisExt as _;

impl KafkaView {
    /// 渲染消息查询页；窄屏把结果区放入页面滚动范围，保证查询控件不会挤掉表格和详情。
    pub(super) fn render_messages(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let stacked_root = f32::from(window.viewport_size().width) < 900.0;
        let compact = f32::from(window.viewport_size().width) < 1280.0;
        let page = self.message_page.as_ref();
        let page_count = self.message_page_count();
        let current_page = self.message_page_index.min(page_count.saturating_sub(1));
        let selected_record = page.and_then(|page| {
            self.selected_message
                .and_then(|index| page.records.get(index))
        });
        let rows = if self.loading_messages {
            v_flex()
                .id("kafka-message-loading")
                .debug_selector(|| "kafka-message-loading".into())
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().small().color(theme.accent))
                .child("正在读取 Kafka 消息…")
                .into_any_element()
        } else if let Some(page) = page {
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
                let page_start = current_page.saturating_mul(self.message_page_size);
                let page_end = page_start
                    .saturating_add(self.message_page_size)
                    .min(page.records.len());
                let records = page.records[page_start..page_end].to_vec();
                let header = message_table_header(&theme);
                let body = uniform_list(
                    "kafka-message-list",
                    records.len(),
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|index| {
                                let record = records[index].clone();
                                let record_index = page_start + index;
                                let selected = this.selected_message == Some(record_index);
                                this.render_message_row(record_index, record, selected, cx)
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.message_scroll)
                .w_full()
                .min_w(px(MESSAGE_TABLE_MIN_WIDTH))
                .flex_1();
                let table_content = v_flex()
                    .w_full()
                    .min_w(px(MESSAGE_TABLE_MIN_WIDTH))
                    .h_full()
                    .child(header)
                    .child(body);
                let table = div()
                    .id("kafka-message-table")
                    .debug_selector(|| "kafka-message-table".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        div()
                            .id("kafka-message-h-scroll")
                            .debug_selector(|| "kafka-message-h-scroll".into())
                            .size_full()
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .track_scroll(&self.message_horizontal_scroll)
                            .child(table_content),
                    )
                    .child(
                        div()
                            .id("kafka-message-v-scrollbar")
                            .debug_selector(|| "kafka-message-v-scrollbar".into())
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(16.0))
                            .bg(theme.scrollbar)
                            .child(
                                Scrollbar::vertical(&self.message_scroll)
                                    .id("kafka-message-v-scrollbar-control")
                                    .scrollbar_show(ScrollbarShow::Always),
                            ),
                    );
                let horizontal_scrollbar = div()
                    .id("kafka-message-h-scrollbar")
                    .debug_selector(|| "kafka-message-h-scrollbar".into())
                    .flex_none()
                    .w_full()
                    .h(px(16.0))
                    .relative()
                    .bg(theme.scrollbar)
                    .child(
                        Scrollbar::horizontal(&self.message_horizontal_scroll)
                            .id("kafka-message-h-scrollbar-control")
                            .scroll_size(gpui::size(px(MESSAGE_TABLE_MIN_WIDTH), px(16.0)))
                            .scrollbar_show(ScrollbarShow::Always),
                    );
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(table)
                    .child(horizontal_scrollbar)
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
            .map(|record| {
                self.render_message_detail(record, compact, cx)
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                v_flex()
                    .when(compact, |view| view.w_full().flex_1().min_w_0())
                    .when(!compact, |view| view.w(px(360.0)).flex_none())
                    .min_h_0()
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
        let pagination = page.map(|page| {
            self.render_message_pagination(page.records.len(), current_page, page_count, cx)
        });
        v_flex()
            .id("kafka-messages")
            .debug_selector(|| "kafka-messages".into())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .when(stacked_root, |page| {
                page.overflow_y_scroll()
                    .track_scroll(&self.message_page_scroll)
            })
            .p(px(18.0))
            .gap(px(12.0))
            .child(self.render_message_controls(window, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .when(compact, |row| row.flex_col())
                    .when(stacked_root, |row| {
                        row.min_h(px(COMPACT_MESSAGE_RESULTS_HEIGHT))
                    })
                    .gap(px(14.0))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .border_1()
                            .border_color(theme.border)
                            .rounded(px(6.0))
                            .child(rows)
                            .when_some(pagination, |panel, pagination| panel.child(pagination)),
                    )
                    .child(detail),
            )
    }

    pub(super) fn render_message_controls(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let compact = f32::from(window.viewport_size().width) < 1080.0;
        let range_modes = [KafkaRangeMode::Offset, KafkaRangeMode::Time]
            .into_iter()
            .fold(
                h_flex()
                    .debug_selector(|| "kafka-range-mode".into())
                    .gap(px(4.0)),
                |row, mode| {
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
                },
            );
        let range_inputs = match self.range_mode {
            KafkaRangeMode::Offset => h_flex()
                .debug_selector(|| "kafka-range-inputs".into())
                .flex_none()
                .items_end()
                .gap(px(8.0))
                .child(
                    field(
                        "起始 Offset",
                        Input::new(&self.start_offset_input).small(),
                        if compact { 0.0 } else { 130.0 },
                    )
                    .debug_selector(|| "kafka-range-start-field".into()),
                )
                .child(
                    field(
                        "结束 Offset",
                        Input::new(&self.end_offset_input).small(),
                        if compact { 0.0 } else { 130.0 },
                    )
                    .debug_selector(|| "kafka-range-end-field".into()),
                )
                .when(compact, |inputs| inputs.w_full().flex_col().items_stretch())
                .into_any_element(),
            KafkaRangeMode::Time => h_flex()
                .debug_selector(|| "kafka-range-inputs".into())
                .flex_none()
                .items_end()
                .gap(px(8.0))
                .child(
                    field(
                        "起始时间",
                        Input::new(&self.start_time_input).small(),
                        if compact { 0.0 } else { 230.0 },
                    )
                    .debug_selector(|| "kafka-range-start-field".into()),
                )
                .child(
                    field(
                        "结束时间",
                        Input::new(&self.end_time_input).small(),
                        if compact { 0.0 } else { 230.0 },
                    )
                    .debug_selector(|| "kafka-range-end-field".into()),
                )
                .when(compact, |inputs| inputs.w_full().flex_col().items_stretch())
                .into_any_element(),
        };
        let message_actions = h_flex()
            .debug_selector(|| "kafka-message-actions".into())
            .flex_none()
            .items_end()
            .gap(px(8.0))
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
                    .loading(self.loading_messages)
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
            });
        let message_actions =
            message_actions.when(compact, |row| row.w_full().flex_wrap().items_end());
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
        // 将查询条件和操作控件分成可收缩的布局组，避免固定宽度控件把搜索区域推出窗口。
        let message_query = h_flex()
            .debug_selector(|| "kafka-message-query".into())
            .flex_1()
            .min_w_0()
            .flex_wrap()
            .items_end()
            .gap(px(8.0))
            .child(field(
                "Topic",
                Input::new(&self.topic_input).small(),
                if compact { 0.0 } else { 190.0 },
            ))
            .child(field(
                "Partition",
                Input::new(&self.partition_input).small(),
                if compact { 0.0 } else { 130.0 },
            ))
            .child(field(
                "范围",
                range_modes,
                if compact { 0.0 } else { 134.0 },
            ))
            .child(range_inputs)
            .when(compact, |query| query.w_full().flex_col().items_stretch());
        let message_search = v_flex()
            .debug_selector(|| "kafka-message-search".into())
            .when(compact, |search| search.w_full())
            .when(!compact, |search| search.w(px(260.0)))
            .flex_none()
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
            );
        let message_search_fields = v_flex()
            .debug_selector(|| "kafka-message-search-fields".into())
            .when(compact, |fields| fields.w_full())
            .when(!compact, |fields| fields.w(px(208.0)))
            .flex_none()
            .gap(px(5.0))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("搜索字段"),
            )
            .child(search_fields);
        v_flex()
            .w_full()
            .flex_none()
            .gap(px(8.0))
            .child(
                h_flex()
                    .debug_selector(|| "kafka-message-query-row".into())
                    .w_full()
                    .min_w_0()
                    .items_end()
                    .gap(px(8.0))
                    .when(compact, |row| row.flex_col().items_stretch())
                    .child(message_query)
                    .child(message_actions),
            )
            .child(
                h_flex()
                    .debug_selector(|| "kafka-message-search-row".into())
                    .w_full()
                    .min_w_0()
                    .items_end()
                    .gap(px(8.0))
                    .when(compact, |row| row.flex_col().items_stretch())
                    .child(message_search)
                    .child(message_search_fields)
                    .child(
                        div()
                            .debug_selector(|| "kafka-message-search-note".into())
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .text_right()
                            .truncate()
                            .child("只读扫描 · 不提交 Offset"),
                    ),
            )
    }

    /// 显示已加载消息的分页状态；翻页只切换内存中的有界结果，不会隐式扩大 Broker 扫描。
    fn render_message_pagination(
        &self,
        total_records: usize,
        current_page: usize,
        page_count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let has_previous = current_page > 0;
        let has_next = current_page.saturating_add(1) < page_count;
        let previous_page = current_page.saturating_sub(1);
        let next_page = current_page.saturating_add(1);
        h_flex()
            .id("kafka-message-pagination")
            .debug_selector(|| "kafka-message-pagination".into())
            .w_full()
            .h(px(38.0))
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(format!("已加载 {total_records} 条消息"))
            .child(div().flex_1().min_w_0())
            .child(
                ramag_ui::clickable_button("kafka-message-page-previous")
                    .debug_selector(|| "kafka-message-page-previous".into())
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .label("上页")
                    .disabled(!has_previous)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_message_page(previous_page, cx);
                    })),
            )
            .child(
                div()
                    .id("kafka-message-page-indicator")
                    .debug_selector(|| "kafka-message-page-indicator".into())
                    .flex_none()
                    .child(if page_count == 0 {
                        "第 0 / 0 页".to_string()
                    } else {
                        format!("第 {} / {} 页", current_page + 1, page_count)
                    }),
            )
            .child(
                ramag_ui::clickable_button("kafka-message-page-next")
                    .debug_selector(|| "kafka-message-page-next".into())
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .label("下页")
                    .disabled(!has_next)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_message_page(next_page, cx);
                    })),
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
            .debug_selector(move || format!("kafka-message-row-{index}"))
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
}
