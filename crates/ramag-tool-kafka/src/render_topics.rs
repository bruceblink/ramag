use super::*;

impl KafkaView {
    pub(super) fn render_topics(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let compact = f32::from(window.viewport_size().width) < 1200.0;
        // The split gets less height than the whole window; derive its cap from the
        // current viewport so the detail actions stay visible at every supported size.
        let split_max_height = (f32::from(window.viewport_size().height) - 540.0).max(240.0);
        let query = value(&self.topic_search, cx).to_lowercase();
        let visible: Vec<KafkaTopic> = self
            .topics
            .iter()
            .filter(|topic| query.is_empty() || topic.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        let page_count = visible.len().div_ceil(self.topic_page_size);
        let current_page = self.topic_page_index.min(page_count.saturating_sub(1));
        let page_start = current_page.saturating_mul(self.topic_page_size);
        let page_end = page_start
            .saturating_add(self.topic_page_size)
            .min(visible.len());
        let selected_topic = self
            .selected_topic
            .as_ref()
            .and_then(|name| self.topics.iter().find(|topic| &topic.name == name));
        let admin_disabled = !self.read_only.allows_admin()
            || self.topic_operation
            || self.loading_runtime
            || self.saving
            || self.deleting;
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
            let topics = visible[page_start..page_end].to_vec();
            let rows = uniform_list(
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
            .min_h_0()
            .track_scroll(&self.topic_scroll);
            let table = div()
                .id("kafka-topic-table")
                .debug_selector(|| "kafka-topic-table".into())
                .relative()
                .size_full()
                .child(v_flex().size_full().child(rows))
                .child(
                    div()
                        .id("kafka-topic-v-scrollbar")
                        .debug_selector(|| "kafka-topic-v-scrollbar".into())
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(px(16.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::vertical(&self.topic_scroll)
                                .id("kafka-topic-v-scrollbar-control")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                );
            div()
                .debug_selector(|| "kafka-topic-list-viewport".into())
                .relative()
                .h_full()
                .flex_1()
                .min_h_0()
                .child(table)
                .into_any_element()
        };
        let detail = selected_topic
            .map(|topic| {
                self.render_topic_detail(topic, compact, window, cx)
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                v_flex()
                    .when(compact, |panel| panel.w_full().flex_1().min_w_0())
                    .when(!compact, |panel| panel.w(px(390.0)).flex_none())
                    .min_h_0()
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
            .debug_selector(|| "kafka-topics".into())
            .size_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(18.0))
            .gap(px(12.0))
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        v_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child("Topics"),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child("Topic 与 Partition 元数据来自 Broker"),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .when(compact, |input| input.flex_1())
                            .when(!compact, |input| input.w(px(260.0)).flex_none())
                            .child(
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
                v_flex()
                    .id("kafka-topic-admin")
                    .debug_selector(|| "kafka-topic-admin".into())
                    .w_full()
                    .flex_none()
                    .gap(px(9.0))
                    .p(px(12.0))
                    .border_1()
                    .border_color(if self.read_only.allows_admin() {
                        theme.warning.opacity(0.45)
                    } else {
                        theme.border
                    })
                    .rounded(px(6.0))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .when(compact, |row| row.flex_col().items_start())
                            .gap(px(12.0))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .when(compact, |copy| copy.flex_initial().w_full())
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Topic 管理"),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(if self.read_only.allows_admin() {
                                                "已启用管理模式；提交前会显示精确确认内容"
                                            } else {
                                                "当前为只读保护；请在配置页开启管理模式"
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if self.read_only.allows_admin() {
                                        theme.warning
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(if self.read_only.allows_admin() {
                                        "可创建 / 删除 / 扩容"
                                    } else {
                                        "写操作已禁用"
                                    }),
                            ),
                    )
                    .child(field(
                        "新建 Topic",
                        Input::new(&self.topic_create_name).small(),
                        0.0,
                    ))
                    .child(
                        h_flex()
                            .w_full()
                            .when(compact, |row| row.flex_col().items_stretch())
                            .when(!compact, |row| row.items_end())
                            .gap(px(10.0))
                            .child(field(
                                "初始 Partition 数量",
                                Input::new(&self.topic_create_partitions).small(),
                                if compact { 0.0 } else { 150.0 },
                            ))
                            .child(field(
                                "副本因子",
                                Input::new(&self.topic_create_replication_factor).small(),
                                if compact { 0.0 } else { 120.0 },
                            ))
                            .child(
                                ramag_ui::clickable_button("kafka-topic-create")
                                    .debug_selector(|| "kafka-topic-create".into())
                                    .primary()
                                    .small()
                                    .icon(IconName::Plus)
                                    .label("创建 Topic")
                                    .when(compact, |button| button.w_full())
                                    .disabled(admin_disabled)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.begin_create_topic(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "单次请求上限：{} 个 Partition、{} 个副本",
                                MAX_KAFKA_PARTITIONS, MAX_KAFKA_REPLICAS
                            )),
                    ),
            )
            .child(
                h_flex()
                    .debug_selector(|| "kafka-topic-split".into())
                    .flex_none()
                    .min_h_0()
                    .max_h(px(split_max_height))
                    .h(px(split_max_height))
                    .when(compact, |layout| layout.flex_col())
                    .items_stretch()
                    .gap(px(14.0))
                    .child(
                        v_flex()
                            .debug_selector(|| "kafka-topic-list-panel".into())
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .border_1()
                            .border_color(theme.border)
                            .rounded(px(6.0))
                            .child(list)
                            .when(page_count > 0, |panel| {
                                panel.child(self.render_topic_pagination(
                                    visible.len(),
                                    current_page,
                                    page_count,
                                    cx,
                                ))
                            }),
                    )
                    .child(detail),
            )
    }

    /// Reset Topic pagination and the list viewport when the cluster or filter changes.
    pub(super) fn reset_topic_paging(&mut self) {
        self.topic_page_index = 0;
        self.topic_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
    }

    /// Switch the already loaded Topic snapshot to a bounded page.
    fn set_topic_page(&mut self, page_index: usize, cx: &mut Context<Self>) {
        if page_index == self.topic_page_index {
            return;
        }
        self.topic_page_index = page_index;
        self.topic_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        cx.notify();
    }

    /// Render pagination for the filtered Topic snapshot without changing Broker state.
    fn render_topic_pagination(
        &self,
        total_topics: usize,
        current_page: usize,
        page_count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let previous_page = current_page.saturating_sub(1);
        let next_page = current_page.saturating_add(1);
        h_flex()
            .id("kafka-topic-pagination")
            .debug_selector(|| "kafka-topic-pagination".into())
            .w_full()
            .h(px(38.0))
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(format!("共 {total_topics} 个 Topic"))
            .child(div().flex_1().min_w_0())
            .child(
                ramag_ui::clickable_button("kafka-topic-page-previous")
                    .debug_selector(|| "kafka-topic-page-previous".into())
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip("上一页")
                    .disabled(current_page == 0)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_topic_page(previous_page, cx);
                    })),
            )
            .child(
                div()
                    .id("kafka-topic-page-indicator")
                    .debug_selector(|| "kafka-topic-page-indicator".into())
                    .flex_none()
                    .child(format!("第 {} / {} 页", current_page + 1, page_count)),
            )
            .child(
                ramag_ui::clickable_button("kafka-topic-page-next")
                    .debug_selector(|| "kafka-topic-page-next".into())
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .tooltip("下一页")
                    .disabled(current_page.saturating_add(1) >= page_count)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_topic_page(next_page, cx);
                    })),
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
        let debug_name = name.clone();
        let copy_debug_name = debug_name.clone();
        let name_for_copy = name.clone();
        h_flex()
            .id(SharedString::from(format!("kafka-topic-row-{name}")))
            .debug_selector(move || format!("kafka-topic-row-{debug_name}"))
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
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                // Ctrl/Command + double-click copies the stable Topic name without changing selection.
                if event.modifiers().secondary() {
                    if ramag_ui::is_primary_modifier_double_click(event) {
                        ramag_ui::copy_text_with_notification(name.clone(), window, cx);
                    }
                    return;
                }
                this.select_topic(name.clone(), window, cx);
            }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(div().text_sm().truncate().child(topic.name))
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
            .child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "kafka-topic-copy-{copy_debug_name}"
                )))
                .debug_selector(move || format!("kafka-topic-copy-{copy_debug_name}"))
                .ghost()
                .xsmall()
                .icon(IconName::Copy)
                .tooltip("复制 Topic 名称")
                .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                    ramag_ui::copy_text_with_notification(name_for_copy.clone(), window, cx);
                })),
            )
    }
}
