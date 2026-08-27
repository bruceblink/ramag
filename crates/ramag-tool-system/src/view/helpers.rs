//! 系统工具视图的通用格式化和重复布局。

use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{Icon, h_flex, v_flex};

use super::Notice;
use crate::{DiskSnapshot, TerminateResult};

pub(super) fn notice_for_termination(result: TerminateResult) -> Notice {
    match result {
        TerminateResult::RefusedSelf { pid } => Notice {
            message: format!("已拒绝终止当前 Ramag 进程（PID {pid}）"),
            error: true,
        },
        TerminateResult::Missing { pid } => Notice {
            message: format!("进程 {pid} 已不存在"),
            error: true,
        },
        TerminateResult::Changed {
            pid,
            expected_name,
            actual_name,
        } => Notice {
            message: format!("进程 {pid} 名称已变化：{expected_name} -> {actual_name}，未执行终止"),
            error: true,
        },
        TerminateResult::Sent { pid, name } => Notice {
            message: format!("已向进程 {name}（PID {pid}）发送终止请求"),
            error: false,
        },
        TerminateResult::Failed { pid, name } => Notice {
            message: format!("无法终止进程 {name}（PID {pid}），请检查权限"),
            error: true,
        },
    }
}

pub(super) fn metric_card(
    title: &'static str,
    value: String,
    detail: String,
    icon: Icon,
    accent: gpui::Hsla,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w(px(180.0))
        .min_h(px(102.0))
        .gap(px(6.0))
        .p(px(12.0))
        .bg(theme.secondary)
        .border_1()
        .border_color(theme.border)
        .rounded(px(6.0))
        .child(
            h_flex()
                .items_center()
                .gap(px(6.0))
                .child(icon.text_color(accent))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(title),
                ),
        )
        .child(div().text_base().child(value))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(detail),
        )
}

pub(super) fn panel_heading(
    title: &'static str,
    detail: &'static str,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(detail),
        )
}

pub(super) fn render_core_row(
    index: usize,
    usage: f32,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    let usage = usage.clamp(0.0, 100.0);
    h_flex()
        .items_center()
        .gap(px(7.0))
        .child(
            div()
                .w(px(58.0))
                .text_xs()
                .child(format!("核心 {}", index + 1)),
        )
        .child(
            div()
                .w(px(180.0))
                .h(px(6.0))
                .bg(theme.muted)
                .rounded_full()
                .child(
                    div()
                        .h_full()
                        .w(px(usage * 1.8))
                        .bg(theme.accent)
                        .rounded_full(),
                ),
        )
        .child(
            div()
                .w(px(52.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format_percent(usage as f64)),
        )
}

pub(super) fn render_meter_row(
    label: &'static str,
    percent: f64,
    detail: String,
    accent: gpui::Hsla,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    let percent = percent.clamp(0.0, 100.0);
    h_flex()
        .items_center()
        .gap(px(8.0))
        .child(div().w(px(48.0)).text_xs().child(label))
        .child(
            div()
                .flex_1()
                .h(px(7.0))
                .bg(theme.muted)
                .rounded_full()
                .child(
                    div()
                        .h_full()
                        .w(px((percent * 1.8) as f32))
                        .bg(accent)
                        .rounded_full(),
                ),
        )
        .child(
            div()
                .w(px(150.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(detail),
        )
}

pub(super) fn render_history(
    title: &'static str,
    points: &[[f64; 2]],
    max_value: f64,
    suffix: &'static str,
    accent: gpui::Hsla,
    theme: &gpui_component::theme::Theme,
) -> AnyElement {
    let latest = points.last().map_or(0.0, |point| point[1]);
    let max_value = max_value.max(f64::EPSILON);
    let mut bars = v_flex().h(px(78.0)).justify_end().gap(px(2.0));
    for point in points.iter().rev().take(24) {
        let width = (point[1].max(0.0) / max_value).clamp(0.0, 1.0) * 250.0;
        bars = bars.child(
            div()
                .h(px(2.0))
                .w_full()
                .bg(theme.muted)
                .child(div().h_full().w(px(width as f32)).bg(accent)),
        );
    }
    v_flex()
        .flex_1()
        .min_w(px(280.0))
        .gap(px(6.0))
        .p(px(12.0))
        .bg(theme.secondary)
        .border_1()
        .border_color(theme.border)
        .rounded(px(6.0))
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(accent)
                        .child(format!("{latest:.1}{suffix}")),
                ),
        )
        .child(if points.is_empty() {
            empty_state("等待第一份采样", theme).into_any_element()
        } else {
            bars.into_any_element()
        })
        .into_any_element()
}

pub(super) fn render_disk_row(
    disk: &DiskSnapshot,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap(px(8.0))
        .py(px(5.0))
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .flex_1()
                .min_w(px(140.0))
                .overflow_hidden()
                .text_xs()
                .child(disk.mount_point.clone()),
        )
        .child(
            div()
                .w(px(90.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(disk.file_system.clone()),
        )
        .child(
            div()
                .w(px(220.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{} / {} ({:.0}%)",
                    format_bytes(disk.used_bytes),
                    format_bytes(disk.total_bytes),
                    disk.usage_percent
                )),
        )
}

pub(super) fn process_header(theme: &gpui_component::theme::Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap(px(8.0))
        .px_3()
        .py(px(7.0))
        .bg(theme.muted)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().w(px(70.0)).child("PID"))
        .child(div().flex_1().min_w(px(140.0)).child("进程"))
        .child(div().w(px(84.0)).child("CPU"))
        .child(div().w(px(110.0)).child("内存"))
        .child(div().w(px(42.0)))
}

pub(super) fn empty_state(
    message: &'static str,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    div()
        .w_full()
        .py(px(24.0))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(message)
}

pub(super) fn history_max(points: &[[f64; 2]]) -> f64 {
    points.iter().map(|point| point[1]).fold(1.0, f64::max)
}

pub(super) fn ratio_percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

pub(super) fn format_percent(value: f64) -> String {
    format!("{:.1}%", value.clamp(0.0, 100.0))
}

pub(super) fn format_rate(value: f64) -> String {
    format!("{value:.1} MB/s")
}

pub(super) fn format_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = value as f64;
    let mut index = 0;
    while value >= 1024.0 && index < UNITS.len() - 1 {
        value /= 1024.0;
        index += 1;
    }
    if index == 0 {
        format!("{} {}", value as u64, UNITS[index])
    } else {
        format!("{value:.1} {}", UNITS[index])
    }
}

#[cfg(test)]
mod tests {
    use super::{format_bytes, format_percent, history_max, ratio_percent};

    #[test]
    fn formatters_keep_units_and_bounds() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_percent(150.0), "100.0%");
        assert_eq!(ratio_percent(5, 10), 50.0);
        assert_eq!(history_max(&[[0.0, 2.0], [1.0, 8.0]]), 8.0);
    }
}
