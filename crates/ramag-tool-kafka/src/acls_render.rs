use super::*;
impl KafkaView {
    pub(crate) fn render_acls(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let compact = f32::from(window.viewport_size().width) < 1060.0;
        let split_height = (f32::from(window.viewport_size().height) - 410.0).clamp(230.0, 470.0);
        let filter_disabled = self.selected_cluster_id.is_none()
            || self.loading_runtime
            || self.loading_acls
            || self.acl_operation;
        let admin_disabled = self.selected_cluster_id.is_none()
            || !self.read_only.allows_admin()
            || self.loading_runtime
            || self.loading_acls
            || self.acl_operation
            || self.saving
            || self.deleting;

        let filter_text = h_flex()
            .w_full()
            .gap(px(10.0))
            .when(compact, |row| row.flex_col().items_stretch())
            .child(
                flexible_field(
                    "Principal",
                    Input::new(&self.acl_principal_filter)
                        .small()
                        .disabled(filter_disabled),
                )
                .when(compact, |field| field.flex_initial().w_full()),
            )
            .child(
                flexible_field(
                    "Host",
                    Input::new(&self.acl_host_filter)
                        .small()
                        .disabled(filter_disabled),
                )
                .when(compact, |field| field.flex_initial().w_full()),
            )
            .child(
                flexible_field(
                    "资源名称",
                    Input::new(&self.acl_resource_name_filter)
                        .small()
                        .disabled(filter_disabled),
                )
                .when(compact, |field| field.flex_initial().w_full()),
            );
        let filter_selectors = h_flex()
            .w_full()
            .gap(px(10.0))
            .when(compact, |row| row.flex_col().items_stretch())
            .child(field(
                "Resource Type",
                self.render_acl_filter_resource_types(cx),
                if compact { 0.0 } else { 170.0 },
            ))
            .child(field(
                "Pattern",
                self.render_acl_filter_pattern_types(cx),
                if compact { 0.0 } else { 150.0 },
            ))
            .child(field(
                "Operation",
                self.render_acl_filter_operations(cx),
                if compact { 0.0 } else { 190.0 },
            ))
            .child(field(
                "Permission",
                self.render_acl_filter_permissions(cx),
                if compact { 0.0 } else { 130.0 },
            ));
        let filter_panel = v_flex()
            .id("kafka-acl-filter")
            .debug_selector(|| "kafka-acl-filter".into())
            .w_full()
            .flex_none()
            .gap(px(10.0))
            .p(px(12.0))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(section_heading(
                        "ACL 查询",
                        format!("{} 条已加载规则", self.acls.len()),
                        &theme,
                    ))
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(
                                ramag_ui::clickable_button("kafka-acl-read")
                                    .debug_selector(|| "kafka-acl-read".into())
                                    .outline()
                                    .small()
                                    .icon(IconName::Search)
                                    .label(if self.loading_acls {
                                        "读取中"
                                    } else {
                                        "读取 ACL"
                                    })
                                    .loading(self.loading_acls)
                                    .disabled(filter_disabled)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        if let Some(config) = this.selected_config() {
                                            this.load_acls(config, window, cx);
                                        }
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("kafka-acl-refresh")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Search)
                                    .tooltip("刷新 ACL")
                                    .disabled(filter_disabled)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        if let Some(config) = this.selected_config() {
                                            this.load_acls(config, window, cx);
                                        }
                                    })),
                            ),
                    ),
            )
            .child(filter_text)
            .child(filter_selectors);

        let admin_panel = v_flex()
            .id("kafka-acl-admin")
            .debug_selector(|| "kafka-acl-admin".into())
            .w_full()
            .flex_none()
            .gap(px(10.0))
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
                    .gap(px(12.0))
                    .child(section_heading(
                        "ACL 管理",
                        "创建和删除都需要二次确认",
                        &theme,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if self.read_only.allows_admin() {
                                theme.warning
                            } else {
                                theme.muted_foreground
                            })
                            .child(if self.read_only.allows_admin() {
                                "管理已启用"
                            } else {
                                "只读保护"
                            }),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(px(10.0))
                    .when(compact, |row| row.flex_col().items_stretch())
                    .child(
                        flexible_field(
                            "Principal",
                            Input::new(&self.acl_principal)
                                .small()
                                .disabled(admin_disabled),
                        )
                        .when(compact, |field| field.flex_initial().w_full()),
                    )
                    .child(
                        flexible_field(
                            "Host",
                            Input::new(&self.acl_host).small().disabled(admin_disabled),
                        )
                        .when(compact, |field| field.flex_initial().w_full()),
                    )
                    .child(
                        flexible_field(
                            "资源名称",
                            Input::new(&self.acl_resource_name)
                                .small()
                                .disabled(admin_disabled),
                        )
                        .when(compact, |field| field.flex_initial().w_full()),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(px(10.0))
                    .when(compact, |row| row.flex_col().items_stretch())
                    .child(field(
                        "Resource Type",
                        self.render_acl_resource_types(cx),
                        if compact { 0.0 } else { 170.0 },
                    ))
                    .child(field(
                        "Pattern",
                        self.render_acl_pattern_types(cx),
                        if compact { 0.0 } else { 150.0 },
                    ))
                    .child(field(
                        "Operation",
                        self.render_acl_operations(cx),
                        if compact { 0.0 } else { 190.0 },
                    ))
                    .child(field(
                        "Permission",
                        self.render_acl_permissions(cx),
                        if compact { 0.0 } else { 130.0 },
                    ))
                    .child(
                        ramag_ui::clickable_button("kafka-acl-create")
                            .debug_selector(|| "kafka-acl-create".into())
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label(if self.acl_operation {
                                "提交中"
                            } else {
                                "创建 ACL"
                            })
                            .when(compact, |button| button.w_full())
                            .disabled(admin_disabled)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.begin_create_acl(window, cx);
                            })),
                    ),
            );

        let list_body = if self.loading_acls {
            v_flex()
                .id("kafka-acl-loading")
                .debug_selector(|| "kafka-acl-loading".into())
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(Spinner::new().small().color(theme.accent))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("正在读取 ACL…"),
                )
                .into_any_element()
        } else if let Some(error) = &self.acl_error {
            v_flex()
                .id("kafka-acl-error")
                .debug_selector(|| "kafka-acl-error".into())
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
        } else if !self.acls_loaded || self.acls.is_empty() {
            v_flex()
                .id("kafka-acl-empty")
                .debug_selector(|| "kafka-acl-empty".into())
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(Icon::new(IconName::Network).text_color(theme.muted_foreground))
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    if self.acls_loaded {
                        "当前筛选条件没有匹配的 ACL"
                    } else {
                        "点击“读取 ACL”获取真实规则"
                    },
                ))
                .into_any_element()
        } else {
            let acls = self.acls.clone();
            uniform_list(
                "kafka-acl-list",
                acls.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|index| acls.get(index).cloned())
                        .map(|acl| {
                            let selected = this.selected_acl.as_ref() == Some(&acl);
                            this.render_acl_row(acl, selected, cx).into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.acl_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let list_panel = v_flex()
            .id("kafka-acl-list-panel")
            .debug_selector(|| "kafka-acl-list-panel".into())
            .min_w_0()
            .min_h_0()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .when(compact, |panel| panel.w_full().h(px(230.0)).flex_none())
            .when(!compact, |panel| panel.flex_1().h_full())
            .child(list_body);
        let detail = self
            .selected_acl
            .as_ref()
            .map(|acl| self.render_acl_detail(acl, window, cx).into_any_element())
            .unwrap_or_else(|| {
                v_flex()
                    .id("kafka-acl-detail-empty")
                    .debug_selector(|| "kafka-acl-detail-empty".into())
                    .min_w_0()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .when(compact, |panel| panel.w_full().h(px(260.0)).flex_none())
                    .when(!compact, |panel| panel.flex_1().h_full())
                    .child(Icon::new(IconName::Network).text_color(theme.muted_foreground))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("选择 ACL 查看完整规则"),
                    )
                    .into_any_element()
            });
        let split = h_flex()
            .id("kafka-acl-split")
            .debug_selector(|| "kafka-acl-split".into())
            .w_full()
            .min_w_0()
            .min_h_0()
            .gap(px(12.0))
            .when(compact, |layout| layout.flex_col().flex_none())
            .when(!compact, |layout| layout.flex_1().h(px(split_height)))
            .child(list_panel)
            .child(detail);

        v_flex()
            .id("kafka-acls")
            .debug_selector(|| "kafka-acls".into())
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(18.0))
            .gap(px(12.0))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .max_w(px(1400.0))
                    .gap(px(12.0))
                    .child(h_flex().w_full().items_center().child(section_heading(
                        "Kafka ACL",
                        "规则读取来自 Broker；删除操作使用完整规则匹配",
                        &theme,
                    )))
                    .child(filter_panel)
                    .child(admin_panel)
                    .child(split),
            )
    }

    fn render_acl_row(
        &self,
        acl: KafkaAcl,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected_acl = acl.clone();
        let row_key = acl_row_key(&acl);
        let row_debug_key = row_key.clone();
        let host = acl.host.clone();
        h_flex()
            .id(SharedString::from(format!("kafka-acl-row-{row_key}")))
            .debug_selector(move || format!("kafka-acl-row-{row_debug_key}"))
            .w_full()
            .min_w_0()
            .items_center()
            .gap(px(8.0))
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
                this.select_acl(selected_acl.clone(), cx);
            }))
            .child(div().size(px(7.0)).rounded_full().bg(if selected {
                theme.accent
            } else {
                theme.muted_foreground
            }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(div().text_sm().truncate().child(acl.principal))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(format!(
                                "{} / {} · {} · {}",
                                acl.resource_type.label(),
                                acl.resource_name,
                                acl.operation.label(),
                                acl.permission.label()
                            )),
                    ),
            )
            .child(
                div()
                    .max_w(px(110.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(host),
            )
    }

    fn render_acl_detail(
        &self,
        acl: &KafkaAcl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let compact = f32::from(window.viewport_size().width) < 1060.0;
        let delete_disabled = !self.read_only.allows_admin()
            || self.acl_operation
            || self.loading_acls
            || self.loading_runtime
            || self.saving
            || self.deleting;
        v_flex()
            .id("kafka-acl-detail")
            .debug_selector(|| "kafka-acl-detail".into())
            .min_w_0()
            .min_h_0()
            .gap(px(10.0))
            .p(px(14.0))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .when(compact, |panel| panel.w_full().h(px(260.0)).flex_none())
            .when(!compact, |panel| panel.flex_1().h_full())
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("ACL 详情"),
                    )
                    .child(
                        ramag_ui::clickable_button("kafka-acl-delete")
                            .debug_selector(|| "kafka-acl-delete".into())
                            .danger()
                            .small()
                            .icon(IconName::Delete)
                            .label("删除")
                            .disabled(delete_disabled)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.begin_delete_acl(window, cx);
                            })),
                    ),
            )
            .child(acl_detail_row("Principal", &acl.principal, &theme))
            .child(acl_detail_row("Host", &acl.host, &theme))
            .child(acl_detail_row(
                "Resource Type",
                acl.resource_type.label(),
                &theme,
            ))
            .child(acl_detail_row("Resource Name", &acl.resource_name, &theme))
            .child(acl_detail_row("Pattern", acl.pattern_type.label(), &theme))
            .child(acl_detail_row("Operation", acl.operation.label(), &theme))
            .child(acl_detail_row("Permission", acl.permission.label(), &theme))
    }
}

fn acl_detail_row(
    label: &'static str,
    value: &str,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .w(px(120.0))
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .truncate()
                .child(value.to_owned()),
        )
}

fn acl_row_key(acl: &KafkaAcl) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        acl.principal,
        acl.resource_type.label(),
        acl.resource_name,
        acl.operation.label(),
        acl.permission.label()
    )
}
