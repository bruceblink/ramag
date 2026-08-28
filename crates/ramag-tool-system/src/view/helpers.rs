//! 系统工具视图的通用格式化和重复布局。

use gpui::{AnyElement, InteractiveElement, IntoElement, ParentElement, Styled, div, px};
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
        .debug_selector(|| format!("system-metric-card-{title}"))
        .flex_1()
        .min_w(px(180.0))
        .h(px(102.0))
        .min_h(px(102.0))
        .gap(px(6.0))
        .p(px(12.0))
        .bg(theme.secondary)
        .border_1()
        .border_color(theme.border)
        .rounded(px(6.0))
        .child(
            h_flex()
                .min_w_0()
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
        .child(
            div()
                .min_w_0()
                .text_base()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(value),
        )
        .child(
            div()
                .debug_selector(|| format!("system-metric-detail-{title}"))
                // 核心数、容量等短详情必须保持完整；只有指标主值允许省略。
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_nowrap()
                .child(detail),
        )
}

pub(super) fn panel_heading(
    title: &'static str,
    detail: &'static str,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .flex_none()
        .min_w_0()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_nowrap()
                .child(detail),
        )
}

/// 将读写速率共用一个单位，避免指标卡因两个单位重复而换行。
pub(super) fn format_rate_pair(
    first_label: &'static str,
    first: f64,
    second_label: &'static str,
    second: f64,
) -> String {
    format!("{first_label} {first:.1} / {second_label} {second:.1} MB/s")
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
                .debug_selector(|| format!("system-core-label-{}", index + 1))
                .w(px(58.0))
                .flex_none()
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
                .debug_selector(|| format!("system-core-percent-{}", index + 1))
                .w(px(52.0))
                .flex_none()
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
    use gpui::{
        AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _,
        TestAppContext, Window, div, px, size,
    };
    use gpui_component::ActiveTheme as _;

    use super::{
        format_bytes, format_percent, format_rate_pair, history_max, ratio_percent, render_core_row,
    };

    struct CoreRowTestHost;

    impl Render for CoreRowTestHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(300.0))
                .child(render_core_row(127, 42.0, cx.theme()))
        }
    }

    #[test]
    fn formatters_keep_units_and_bounds() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_percent(150.0), "100.0%");
        assert_eq!(ratio_percent(5, 10), 50.0);
        assert_eq!(history_max(&[[0.0, 2.0], [1.0, 8.0]]), 8.0);
    }

    #[test]
    fn rate_pairs_use_one_shared_unit() {
        assert_eq!(
            format_rate_pair("读", 27.3, "写", 27.0),
            "读 27.3 / 写 27.0 MB/s"
        );
    }

    #[gpui::test]
    fn core_row_keeps_labels_from_shrinking_with_the_meter(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| CoreRowTestHost);
            gpui_component::Root::new(host, window, cx)
        });
        cx.simulate_resize(size(px(300.0), px(80.0)));
        cx.run_until_parked();

        assert_eq!(
            cx.debug_bounds("system-core-label-128")
                .expect("core label should be rendered")
                .size
                .width,
            px(58.0)
        );
        assert_eq!(
            cx.debug_bounds("system-core-percent-128")
                .expect("core percentage should be rendered")
                .size
                .width,
            px(52.0)
        );
    }
}
