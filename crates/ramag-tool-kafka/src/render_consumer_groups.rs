use super::render_consumer_group_helpers::{
    consumer_member_row, consumer_offset_row, empty_group_message, group_metric,
};
use super::*;

const MAX_VISIBLE_GROUP_OFFSETS: usize = 500;

impl KafkaView {
    /// 触发一次有界消费者组快照读取；旧请求的结果不会覆盖当前集群。
    pub(super) fn load_consumer_groups(
        &mut self,
        config: KafkaClusterConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading_consumer_groups || self.selected_cluster_id.as_ref() != Some(&config.id) {
            return;
        }
        self.consumer_group_request_id = self.consumer_group_request_id.wrapping_add(1);
        let request_id = self.consumer_group_request_id;
        let cluster_id = config.id.clone();
        let service = self.service.clone();
        self.loading_consumer_groups = true;
        self.consumer_group_error = None;
        self.notice = Some(("正在读取 Kafka 消费者组…".into(), false));
        cx.spawn_in(window, async move |this, cx| {
            let result = service.list_consumer_groups(&config).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.consumer_group_request_id != request_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.loading_consumer_groups = false;
                match result {
                    Ok(groups) => {
                        let selected = this.selected_consumer_group.clone().filter(|group_id| {
                            groups.iter().any(|group| &group.group_id == group_id)
                        });
                        let count = groups.len();
                        this.consumer_groups = groups;
                        this.selected_consumer_group = selected.or_else(|| {
                            this.consumer_groups
                                .first()
                                .map(|group| group.group_id.clone())
                        });
                        this.consumer_group_error = None;
                        this.notice = Some((format!("已读取 {count} 个消费者组"), false));
                    }
                    Err(error) => {
                        this.consumer_groups.clear();
                        this.selected_consumer_group = None;
                        this.consumer_group_error = Some(error.user_message());
                        this.notice =
                            Some((format!("读取消费者组失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 使当前消费者组任务失效，刷新或切换集群时丢弃迟到结果。
    pub(super) fn invalidate_consumer_group_request(&mut self) {
        self.consumer_group_request_id = self.consumer_group_request_id.wrapping_add(1);
        self.loading_consumer_groups = false;
    }

    pub(super) fn select_consumer_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if self
            .consumer_groups
            .iter()
            .any(|group| group.group_id == group_id)
        {
            self.selected_consumer_group = Some(group_id);
            cx.notify();
        }
    }

    pub(super) fn render_consumer_groups(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let compact = f32::from(window.viewport_size().width) < 1080.0;
        let query = value(&self.consumer_group_search, cx).to_lowercase();
        let visible: Vec<KafkaConsumerGroup> = self
            .consumer_groups
            .iter()
            .filter(|group| query.is_empty() || group.group_id.to_lowercase().contains(&query))
            .cloned()
            .collect();
        let selected_group = self.selected_consumer_group.as_ref().and_then(|group_id| {
            self.consumer_groups
                .iter()
                .find(|group| &group.group_id == group_id)
        });
        let list_body = if self.loading_consumer_groups {
            v_flex()
                .id("kafka-consumer-group-loading")
                .debug_selector(|| "kafka-consumer-group-loading".into())
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(Spinner::new().small().color(theme.accent))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("正在读取消费者组…"),
                )
                .into_any_element()
        } else if let Some(error) = &self.consumer_group_error {
            v_flex()
                .id("kafka-consumer-group-error")
                .debug_selector(|| "kafka-consumer-group-error".into())
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .px(px(18.0))
                .child(Icon::new(IconName::TriangleAlert).text_color(theme.danger))
                .child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child(format!("读取失败：{error}")),
                )
                .into_any_element()
        } else if visible.is_empty() {
            v_flex()
                .id("kafka-consumer-group-empty")
                .debug_selector(|| "kafka-consumer-group-empty".into())
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(Icon::new(IconName::Network).text_color(theme.muted_foreground))
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    if self.consumer_groups.is_empty() {
                        "当前集群没有消费者组"
                    } else {
                        "没有匹配的消费者组"
                    },
                ))
                .into_any_element()
        } else {
            let groups = visible;
            let list = uniform_list(
                "kafka-consumer-group-list",
                groups.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .map(|index| {
                            let group = groups[index].clone();
                            let selected =
                                this.selected_consumer_group.as_ref() == Some(&group.group_id);
                            this.render_consumer_group_row(group, selected, cx)
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.consumer_group_scroll)
            .flex_1();
            v_flex()
                .id("kafka-consumer-group-list-scroll")
                .debug_selector(|| "kafka-consumer-group-list-scroll".into())
                .relative()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(
                    div()
                        .id("kafka-consumer-group-v-scrollbar")
                        .debug_selector(|| "kafka-consumer-group-v-scrollbar".into())
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(px(16.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::vertical(&self.consumer_group_scroll)
                                .id("kafka-consumer-group-v-scrollbar-control")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                )
                .into_any_element()
        };
        let list_height = (f32::from(window.viewport_size().height) - 360.0).clamp(150.0, 230.0);
        let list_panel = v_flex()
            .id("kafka-consumer-group-list")
            .debug_selector(|| "kafka-consumer-group-list".into())
            .min_w_0()
            .min_h_0()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .when(compact, |panel| {
                panel.w_full().h(px(list_height)).flex_none()
            })
            .when(!compact, |panel| panel.w(px(330.0)).h_full().flex_none())
            .child(
                v_flex()
                    .flex_none()
                    .gap(px(8.0))
                    .p(px(12.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(section_heading(
                                "消费者组",
                                format!("{} 个组", self.consumer_groups.len()),
                                &theme,
                            )),
                    )
                    .child(
                        ramag_ui::cleanable_input(
                            &self.consumer_group_search,
                            "kafka-consumer-group-search-clear",
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
            )
            .child(list_body);
        let detail = selected_group
            .map(|group| {
                self.render_consumer_group_detail(group, cx)
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                v_flex()
                    .id("kafka-consumer-group-detail-empty")
                    .debug_selector(|| "kafka-consumer-group-detail-empty".into())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .child(Icon::new(IconName::Network).text_color(theme.muted_foreground))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("选择消费者组查看成员、分配和 Offset"),
                    )
                    .into_any_element()
            });
        v_flex()
            .id("kafka-consumer-groups")
            .debug_selector(|| "kafka-consumer-groups".into())
            .size_full()
            .min_w_0()
            .min_h_0()
            .p(px(18.0))
            .gap(px(12.0))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .when(compact, |row| row.flex_col().items_stretch())
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("消费者组"),
                            )
                            .child(
                                div().text_xs().text_color(theme.muted_foreground).child(
                                    "查看成员分配、已提交 Offset 和 Lag；此页不会提交消费位点",
                                ),
                            ),
                    )
                    .child(
                        ramag_ui::clickable_button("kafka-consumer-groups-refresh")
                            .debug_selector(|| "kafka-consumer-groups-refresh".into())
                            .outline()
                            .small()
                            .icon(IconName::Search)
                            .label("刷新")
                            .when(compact, |button| button.w_full())
                            .loading(self.loading_consumer_groups)
                            .disabled(self.loading_consumer_groups || self.loading_runtime)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                if let Some(config) = this.selected_config() {
                                    this.load_consumer_groups(config, window, cx);
                                }
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .items_stretch()
                    .gap(px(12.0))
                    .when(compact, |row| row.flex_col())
                    .child(list_panel)
                    .child(detail),
            )
    }

    fn render_consumer_group_row(
        &self,
        group: KafkaConsumerGroup,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let group_id = group.group_id.clone();
        let debug_id = group_id.clone();
        let copy_id = group_id.clone();
        h_flex()
            .id(SharedString::from(format!(
                "kafka-consumer-group-row-{group_id}"
            )))
            .debug_selector(move || format!("kafka-consumer-group-row-{debug_id}"))
            .w_full()
            .min_w_0()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(theme.border)
            .when(selected, |row| row.bg(theme.accent.opacity(0.1)))
            .when(!selected, |row| {
                row.hover(|row| row.bg(theme.muted.opacity(0.5)))
            })
            .cursor_pointer()
            .on_click(cx.listener({
                let group_id = group_id.clone();
                move |this, _: &ClickEvent, _, cx| {
                    this.select_consumer_group(group_id.clone(), cx);
                }
            }))
            .child(div().size(px(7.0)).rounded_full().bg(if selected {
                theme.accent
            } else {
                theme.muted_foreground
            }))
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap(px(2.0))
                    .child(div().text_sm().truncate().child(group.group_id))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .min_w_0()
                            .truncate()
                            .child(format!(
                                "{} 成员 · {} Offset",
                                group.members.len(),
                                group.offsets.len()
                            )),
                    ),
            )
            .child(
                div()
                    .max_w(px(96.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(group.state.unwrap_or_else(|| "未知".into())),
            )
            .child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "kafka-consumer-group-copy-{copy_id}"
                )))
                .debug_selector(move || format!("kafka-consumer-group-copy-{copy_id}"))
                .ghost()
                .xsmall()
                .icon(IconName::Copy)
                .tooltip("复制消费者组 ID")
                .on_click(cx.listener({
                    let group_id = group_id.clone();
                    move |_, _: &ClickEvent, window, cx| {
                        ramag_ui::copy_text_with_notification(group_id.clone(), window, cx);
                    }
                })),
            )
    }

    fn render_consumer_group_detail(
        &self,
        group: &KafkaConsumerGroup,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let offset_count = group.offsets.len();
        let assignment_count = group
            .members
            .iter()
            .map(|member| member.assigned_partitions.len())
            .sum::<usize>();
        let mut member_rows = v_flex()
            .id("kafka-consumer-group-members")
            .debug_selector(|| "kafka-consumer-group-members".into())
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0));
        if group.members.is_empty() {
            member_rows = member_rows.child(empty_group_message("当前没有活跃成员", &theme));
        } else {
            for member in &group.members {
                member_rows = member_rows.child(consumer_member_row(member, &theme));
            }
        }
        let mut offset_rows = v_flex()
            .id("kafka-consumer-group-offset-rows")
            .debug_selector(|| "kafka-consumer-group-offset-rows".into())
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0));
        for offset in group.offsets.iter().take(MAX_VISIBLE_GROUP_OFFSETS) {
            offset_rows = offset_rows.child(consumer_offset_row(offset, &theme));
        }
        if group.offsets.is_empty() {
            offset_rows = offset_rows.child(empty_group_message("当前没有已提交 Offset", &theme));
        } else if group.offsets.len() > MAX_VISIBLE_GROUP_OFFSETS {
            offset_rows = offset_rows.child(
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_xs()
                    .text_color(theme.warning)
                    .child(format!(
                        "Offset 数量过多，仅展示前 {} 条",
                        MAX_VISIBLE_GROUP_OFFSETS
                    )),
            );
        }
        v_flex()
            .id("kafka-consumer-group-detail")
            .debug_selector(|| "kafka-consumer-group-detail".into())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap(px(10.0))
            .p(px(2.0))
            .overflow_y_scroll()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("kafka-consumer-group-detail-name")
                            .debug_selector(|| "kafka-consumer-group-detail-name".into())
                            .flex_1()
                            .min_w_0()
                            .child(
                                ramag_ui::SelectableText::new(
                                    "kafka-consumer-group-detail-selectable-name",
                                    group.group_id.clone(),
                                )
                                .w_full()
                                .text_sm(),
                            ),
                    )
                    .child(
                        ramag_ui::clickable_button("kafka-consumer-group-detail-copy")
                            .debug_selector(|| "kafka-consumer-group-detail-copy".into())
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("复制消费者组 ID")
                            .on_click(cx.listener({
                                let group_id = group.group_id.clone();
                                move |_, _: &ClickEvent, window, cx| {
                                    ramag_ui::copy_text_with_notification(
                                        group_id.clone(),
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(group_metric(
                        "状态",
                        group.state.as_deref().unwrap_or("未知"),
                        &theme,
                    ))
                    .child(group_metric(
                        "协议",
                        group.protocol.as_deref().unwrap_or("未知"),
                        &theme,
                    ))
                    .child(group_metric(
                        "成员",
                        &group.members.len().to_string(),
                        &theme,
                    ))
                    .child(group_metric("Offset", &offset_count.to_string(), &theme))
                    .child(group_metric(
                        "分配 Partition",
                        &assignment_count.to_string(),
                        &theme,
                    )),
            )
            .child(section_heading(
                "成员与分配",
                "当前活跃成员以及 Kafka 分配的 Topic/Partition",
                &theme,
            ))
            .child(member_rows)
            .child(section_heading(
                "已提交 Offset",
                "Lag = 末尾 Offset - 已提交 Offset；未知值保留为未知",
                &theme,
            ))
            .child(offset_rows)
    }
}
