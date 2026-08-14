use gpui::{IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use ramag_domain::entities::{DataSyncSummary, format_bytes};

#[allow(clippy::too_many_arguments)]
pub(super) fn render_result_summary(
    summary: &DataSyncSummary,
    objects_total: Option<u64>,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    foreground: gpui::Hsla,
    success: gpui::Hsla,
    warning: gpui::Hsla,
    danger: gpui::Hsla,
) -> impl IntoElement {
    let warning_count = (summary.warnings.len() as u64).saturating_add(summary.warnings_overflow);
    let completed_objects = objects_total.map_or_else(
        || format_count(summary.objects),
        |total| {
            format!(
                "{} / {}",
                format_count(summary.objects),
                format_count(total)
            )
        },
    );
    let failed_color = if summary.failed > 0 {
        danger
    } else {
        foreground
    };
    let warning_color = if warning_count > 0 {
        warning
    } else {
        foreground
    };

    v_flex()
        .id("sync-result-summary")
        .debug_selector(|| "sync-result-summary".into())
        .w_full()
        .gap(px(12.0))
        .p(px(14.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(border)
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(16.0))
                .child(result_metric(
                    "新增记录",
                    format_count(summary.inserted),
                    muted,
                    success,
                ))
                .child(result_metric(
                    "扫描记录",
                    format_count(summary.scanned),
                    muted,
                    foreground,
                ))
                .child(result_metric(
                    "完成对象",
                    completed_objects,
                    muted,
                    foreground,
                )),
        )
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(16.0))
                .child(result_metric(
                    "跳过记录",
                    format_count(summary.skipped),
                    muted,
                    foreground,
                ))
                .child(result_metric(
                    "失败记录",
                    format_count(summary.failed),
                    muted,
                    failed_color,
                ))
                .child(result_metric(
                    "警告",
                    format_count(warning_count),
                    muted,
                    warning_color,
                )),
        )
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(16.0))
                .pt(px(10.0))
                .border_t_1()
                .border_color(border)
                .child(result_metric(
                    "传输量",
                    format_bytes(summary.bytes),
                    muted,
                    foreground,
                ))
                .child(result_metric(
                    "总耗时",
                    format_elapsed_ms(summary.elapsed_ms),
                    muted,
                    foreground,
                )),
        )
}

fn result_metric(
    label: &'static str,
    value: String,
    muted: gpui::Hsla,
    value_color: gpui::Hsla,
) -> gpui::Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(px(2.0))
        .child(div().text_xs().text_color(muted).child(label))
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(value_color)
                .child(value),
        )
}

pub(super) fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

pub(super) fn format_elapsed_ms(elapsed_ms: u64) -> String {
    const MINUTE_MS: u64 = 60_000;
    const HOUR_MS: u64 = 60 * MINUTE_MS;

    let hours = elapsed_ms / HOUR_MS;
    let minutes = (elapsed_ms % HOUR_MS) / MINUTE_MS;
    let seconds_ms = elapsed_ms % MINUTE_MS;
    let seconds = seconds_ms / 1_000;
    let hundredths = (seconds_ms % 1_000) / 10;

    if hours > 0 {
        format!("{hours} 小时 {minutes} 分 {seconds:02}.{hundredths:02} 秒")
    } else if minutes > 0 {
        format!("{minutes} 分 {seconds:02}.{hundredths:02} 秒")
    } else {
        format!("{seconds}.{hundredths:02} 秒")
    }
}
