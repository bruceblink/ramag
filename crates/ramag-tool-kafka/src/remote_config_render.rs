use super::*;

impl KafkaView {
    pub(super) fn render_remote_config(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let compact = f32::from(window.viewport_size().width) < 980.0;
        let read_disabled = self.selected_cluster_id.is_none()
            || self.loading_configs
            || self.updating_config
            || self.saving
            || self.deleting;
        let controls = if compact {
            v_flex()
                .id("kafka-config-query")
                .debug_selector(|| "kafka-config-query".into())
                .w_full()
                .gap(px(10.0))
                .child(field(
                    "资源类型",
                    self.render_config_resource_types(cx),
                    0.0,
                ))
                .child(field(
                    "资源名称",
                    Input::new(&self.config_resource_name).small(),
                    0.0,
                ))
                .child(
                    ramag_ui::clickable_button("kafka-config-read")
                        .debug_selector(|| "kafka-config-read".into())
                        .outline()
                        .small()
                        .icon(IconName::Search)
                        .label(if self.loading_configs {
                            "读取中"
                        } else {
                            "读取配置"
                        })
                        .when(compact, |button| button.w_full())
                        .disabled(read_disabled)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.load_configs(window, cx);
                        })),
                )
                .into_any_element()
        } else {
            h_flex()
                .id("kafka-config-query")
                .debug_selector(|| "kafka-config-query".into())
                .w_full()
                .min_w_0()
                .items_end()
                .gap(px(12.0))
                .child(
                    field("资源类型", self.render_config_resource_types(cx), 250.0)
                        .flex_none()
                        .min_w_0(),
                )
                .child(
                    flexible_field("资源名称", Input::new(&self.config_resource_name).small())
                        .min_w_0(),
                )
                .child(
                    ramag_ui::clickable_button("kafka-config-read")
                        .debug_selector(|| "kafka-config-read".into())
                        .outline()
                        .small()
                        .icon(IconName::Search)
                        .label(if self.loading_configs {
                            "读取中"
                        } else {
                            "读取配置"
                        })
                        .flex_none()
                        .disabled(read_disabled)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.load_configs(window, cx);
                        })),
                )
                .into_any_element()
        };

        let editor = if let Some(key) = self.editing_config_key.clone() {
            let can_modify = self
                .config_entries
                .iter()
                .find(|entry| entry.key == key)
                .is_some_and(|entry| entry.can_modify(self.config_resource_type));
            v_flex()
                .id("kafka-config-editor")
                .debug_selector(|| "kafka-config-editor".into())
                .w_full()
                .min_w_0()
                .gap(px(8.0))
                .p(px(10.0))
                .border_1()
                .border_color(theme.accent.opacity(0.4))
                .rounded(px(6.0))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme.accent)
                                .truncate()
                                .child(format!("准备设置：{key}")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("修改后点击该配置项的“设置”"),
                        ),
                )
                .child(field("新值", Input::new(&self.config_value).small(), 0.0))
                .when(!can_modify, |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(theme.warning)
                            .child("该配置项当前不可修改"),
                    )
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let list_header = if compact {
            div().into_any_element()
        } else {
            h_flex()
                .id("kafka-config-list-header")
                .debug_selector(|| "kafka-config-list-header".into())
                .w_full()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().w(px(240.0)).flex_none().child("配置键"))
                .child(div().flex_1().min_w_0().child("当前值"))
                .child(div().w(px(130.0)).flex_none().child("来源"))
                .child(div().w(px(60.0)).flex_none().child("状态"))
                .child(div().w(px(150.0)).flex_none().child("操作"))
                .into_any_element()
        };

        let list = if self.loading_configs {
            v_flex()
                .id("kafka-config-loading")
                .debug_selector(|| "kafka-config-loading".into())
                .h(px(if compact { 180.0 } else { 240.0 }))
                .items_center()
                .justify_center()
                .child(Spinner::new().small())
                .into_any_element()
        } else if self.config_entries.is_empty() {
            v_flex()
                .id("kafka-config-empty")
                .debug_selector(|| "kafka-config-empty".into())
                .h(px(if compact { 180.0 } else { 240.0 }))
                .items_center()
                .justify_center()
                .gap(px(5.0))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("尚未读取 Kafka 配置"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("选择 Topic 或 Broker，输入资源名称后读取"),
                )
                .into_any_element()
        } else {
            let entries = self.config_entries.clone();
            uniform_list(
                "kafka-config-list",
                entries.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|index| entries.get(index).cloned())
                        .map(|entry| this.render_config_entry(entry, compact, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .id("kafka-config-list")
            .debug_selector(|| "kafka-config-list".into())
            .w_full()
            .min_w_0()
            .h(px(if compact { 340.0 } else { 440.0 }))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .into_any_element()
        };

        v_flex()
            .id("kafka-remote-config")
            .debug_selector(|| "kafka-remote-config".into())
            .w_full()
            .min_w_0()
            .gap(px(10.0))
            .pt(px(10.0))
            .border_t_1()
            .border_color(theme.border)
            .child(section_heading(
                "Kafka 运行配置",
                "配置来源由 Kafka Admin API 返回；只显示当前有效值，敏感值不会回显。",
                &theme,
            ))
            .child(controls)
            .child(editor)
            .child(list_header)
            .child(list)
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                if self.read_only.allows_admin() {
                    "仅动态且非敏感配置可修改；每次设置或删除覆盖前都会再次确认。"
                } else {
                    "当前为只读保护；读取配置不会修改 Kafka，写入入口保持禁用。"
                },
            ))
    }

    fn render_config_resource_types(&self, cx: &mut Context<Self>) -> impl IntoElement {
        [
            KafkaConfigResourceType::Topic,
            KafkaConfigResourceType::Broker,
        ]
        .into_iter()
        .fold(h_flex().flex_wrap().gap(px(4.0)), |row, resource_type| {
            let selected = self.config_resource_type == resource_type;
            row.child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "kafka-config-resource-{}",
                    resource_type.label().to_lowercase()
                )))
                .debug_selector(move || {
                    format!(
                        "kafka-config-resource-{}",
                        resource_type.label().to_lowercase()
                    )
                })
                .small()
                .label(resource_type.label())
                .when(selected, |button| button.primary())
                .when(!selected, |button| button.ghost())
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.select_config_resource_type(resource_type, window, cx);
                    },
                )),
            )
        })
    }

    fn render_config_entry(
        &self,
        entry: KafkaConfigEntry,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let key = entry.key.clone();
        let entry_selector = format!("kafka-config-entry-{key}");
        let can_modify = entry.can_modify(self.config_resource_type);
        let selected = self.editing_config_key.as_deref() == Some(key.as_str());
        let status = if entry.is_sensitive {
            "敏感"
        } else if entry.is_read_only {
            "只读"
        } else if can_modify {
            "可修改"
        } else if entry.is_default {
            "默认"
        } else {
            "不可修改"
        };
        let value = entry.display_value();
        let source = entry.source.label();
        let disabled = !can_modify
            || self.loading_configs
            || self.updating_config
            || !self.read_only.allows_admin();
        let set_key = key.clone();
        let delete_key = key.clone();
        let set_selector = set_key.clone();
        let delete_selector = delete_key.clone();
        let actions = h_flex()
            .gap(px(6.0))
            .when(compact, |row| row.w_full().min_w_0())
            .child(
                ramag_ui::clickable_button(SharedString::from(format!("kafka-config-set-{key}")))
                    .debug_selector(move || format!("kafka-config-set-{set_selector}"))
                    .outline()
                    .small()
                    .label("设置")
                    .when(compact, |button| button.flex_1())
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.begin_config_update(
                            KafkaConfigUpdateOperation::Set,
                            set_key.clone(),
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "kafka-config-delete-{key}"
                )))
                .debug_selector(move || format!("kafka-config-delete-{delete_selector}"))
                .danger()
                .small()
                .label("删除覆盖")
                .when(compact, |button| button.flex_1())
                .disabled(disabled)
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.begin_config_update(
                            KafkaConfigUpdateOperation::Delete,
                            delete_key.clone(),
                            window,
                            cx,
                        );
                    },
                )),
            );

        let row = if compact {
            v_flex()
                .w_full()
                .min_w_0()
                .gap(px(7.0))
                .px(px(12.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(theme.border)
                .when(selected, |row| row.bg(theme.accent.opacity(0.08)))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .child(key.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(if can_modify {
                                    theme.accent
                                } else {
                                    theme.muted_foreground
                                })
                                .child(source),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(value),
                )
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .justify_between()
                        .gap(px(8.0))
                        .when(compact, |row| row.flex_col().items_stretch())
                        .child(
                            div()
                                .flex_none()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(status),
                        )
                        .child(actions),
                )
        } else {
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(9.0))
                .border_b_1()
                .border_color(theme.border)
                .when(selected, |row| row.bg(theme.accent.opacity(0.08)))
                .child(
                    div()
                        .w(px(240.0))
                        .flex_none()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .child(key),
                )
                .child(div().flex_1().min_w_0().text_xs().truncate().child(value))
                .child(
                    div()
                        .w(px(130.0))
                        .flex_none()
                        .min_w_0()
                        .text_xs()
                        .text_color(if can_modify {
                            theme.accent
                        } else {
                            theme.muted_foreground
                        })
                        .truncate()
                        .child(source),
                )
                .child(
                    div()
                        .w(px(60.0))
                        .flex_none()
                        .text_xs()
                        .text_color(if can_modify {
                            theme.accent
                        } else {
                            theme.muted_foreground
                        })
                        .child(status),
                )
                .child(actions)
        };
        row.debug_selector(move || entry_selector.clone())
            .into_any_element()
    }
}
