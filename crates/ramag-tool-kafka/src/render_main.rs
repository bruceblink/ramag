use super::*;

impl KafkaView {
    pub(super) fn render_main(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected = self.selected_config();
        let title = selected
            .as_ref()
            .map(|config| config.name.clone())
            .unwrap_or_else(|| "新建 Kafka 集群".into());
        let status = if self.loading_runtime {
            ("同步中", theme.warning)
        } else if self.metadata.is_some() {
            ("已连接", theme.success)
        } else {
            ("未连接", theme.muted_foreground)
        };
        let admin_mode_label = if self.read_only.allows_admin() {
            "管理已启用"
        } else {
            "只读"
        };
        let admin_mode_color = if self.read_only.allows_admin() {
            theme.warning
        } else {
            theme.muted_foreground
        };
        // 新建草稿尚未分配集群 ID，但必须先显示配置表单；只有初始概览才显示欢迎页。
        let show_welcome = selected.is_none() && self.section != KafkaSection::Config;
        let body = if show_welcome {
            self.render_welcome(cx).into_any_element()
        } else {
            self.render_workspace(window, cx).into_any_element()
        };

        v_flex()
            .id("kafka-main")
            .debug_selector(|| "kafka-main".into())
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.background)
            .child(
                h_flex()
                    .h(px(58.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px(px(22.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .debug_selector(|| "kafka-header-status".into())
                            .flex_1()
                            .min_w_0()
                            .gap(px(10.0))
                            .child(div().size(px(9.0)).rounded_full().bg(status.1))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(status.1)
                                            .truncate()
                                            .child(status.0),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(3.0))
                                    .rounded(px(4.0))
                                    .bg(if self.read_only.allows_admin() {
                                        theme.warning.opacity(0.12)
                                    } else {
                                        theme.muted.opacity(0.65)
                                    })
                                    .text_xs()
                                    .text_color(admin_mode_color)
                                    .child(admin_mode_label),
                            )
                            .when(selected.is_some(), |row| {
                                row.child(
                                    ramag_ui::clickable_button("kafka-test-connection")
                                        .outline()
                                        .small()
                                        .icon(IconName::CircleCheck)
                                        .label("测试连接")
                                        .disabled(
                                            self.testing
                                                || self.saving
                                                || self.deleting
                                                || self.topic_operation
                                                || self.acl_operation,
                                        )
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.test_connection(window, cx);
                                            },
                                        )),
                                )
                            })
                            .child(
                                ramag_ui::clickable_button("kafka-refresh")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Search)
                                    .tooltip("刷新元数据")
                                    .disabled(
                                        self.loading_runtime
                                            || selected.is_none()
                                            || self.testing
                                            || self.saving
                                            || self.deleting
                                            || self.topic_operation
                                            || self.acl_operation,
                                    )
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        if let Some(config) = this.selected_config() {
                                            this.load_runtime(config, window, cx);
                                        }
                                    })),
                            ),
                    ),
            )
            .when_some(self.notice.clone(), |view, notice| {
                view.child(self.render_notice(notice, cx))
            })
            .child(body)
    }

    pub(super) fn render_notice(
        &self,
        notice: (String, bool),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (message, is_error) = notice;
        h_flex()
            .id("kafka-notice")
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(22.0))
            .py(px(8.0))
            .bg(if is_error {
                theme.danger.opacity(0.1)
            } else {
                theme.accent.opacity(0.08)
            })
            .text_xs()
            .text_color(if is_error {
                theme.danger
            } else {
                theme.muted_foreground
            })
            .child(Icon::new(if is_error {
                IconName::TriangleAlert
            } else {
                IconName::CircleCheck
            }))
            .child(message)
    }
}
