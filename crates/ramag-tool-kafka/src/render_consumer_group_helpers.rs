use super::*;

const MAX_VISIBLE_GROUP_ASSIGNMENTS: usize = 200;

pub(crate) fn group_metric(
    label: &'static str,
    value: &str,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w(px(110.0))
        .gap(px(3.0))
        .p(px(10.0))
        .border_1()
        .border_color(theme.border)
        .rounded(px(6.0))
        .bg(theme.secondary.opacity(0.45))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(div().text_sm().truncate().child(value.to_owned()))
}

pub(crate) fn consumer_member_row(
    member: &ramag_domain::entities::KafkaConsumerMember,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let assignments = member
        .assigned_partitions
        .iter()
        .take(MAX_VISIBLE_GROUP_ASSIGNMENTS)
        .map(|assignment| format!("{}-{}", assignment.topic, assignment.partition))
        .collect::<Vec<_>>();
    let assignment_text = if assignments.is_empty() {
        "没有分配".to_owned()
    } else {
        assignments.join(", ")
    };
    v_flex()
        .w_full()
        .gap(px(4.0))
        .px(px(12.0))
        .py(px(9.0))
        .border_b_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(div().text_sm().truncate().child(member.client_id.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(
                            member
                                .client_host
                                .clone()
                                .unwrap_or_else(|| "地址未知".into()),
                        ),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("Member ID：{}", member.member_id)),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .w_full()
                .min_w_0()
                .truncate()
                .child(format!("分配：{assignment_text}")),
        )
}

pub(crate) fn consumer_offset_row(
    offset: &ramag_domain::entities::KafkaConsumerGroupOffset,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let lag_text = display_option_i64(offset.lag);
    let lag_color = match offset.lag {
        Some(0) => theme.success,
        Some(value) if value > 0 => theme.warning,
        Some(_) => theme.danger,
        None => theme.muted_foreground,
    };
    h_flex()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .truncate()
                .child(format!("{} / {}", offset.topic, offset.partition)),
        )
        .child(offset_value("已提交", offset.committed_offset, theme))
        .child(offset_value("末尾", offset.end_offset, theme))
        .child(
            div()
                .w(px(88.0))
                .text_right()
                .text_xs()
                .text_color(lag_color)
                .child(format!("Lag {lag_text}")),
        )
}

fn offset_value(
    label: &'static str,
    value: Option<i64>,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    div()
        .w(px(96.0))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(format!("{label} {}", display_option_i64(value)))
}

pub(crate) fn empty_group_message(
    message: &'static str,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    div()
        .w_full()
        .px(px(12.0))
        .py(px(14.0))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(message)
}
