//! 系统工具视图的通用格式化和重复布局。

use gpui::{AnyElement, InteractiveElement, IntoElement, ParentElement, Styled, div, px, relative};
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
    detail: impl Into<gpui::SharedString>,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    let detail = detail.into();
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

/// 根据核心数量选择近似方形的网格，行列都由固定面板空间平均分配。
pub(super) fn core_grid_dimensions(core_count: usize) -> (usize, usize) {
    if core_count == 0 {
        return (1, 1);
    }
    // 超过 32 个核心时增加列数，优先压低行数；固定面板才能为每个图表保留可见高度。
    let target_area = core_count.saturating_mul(if core_count > 32 { 2 } else { 1 });
    let columns = (target_area as f64).sqrt().ceil() as usize;
    (columns, core_count.div_ceil(columns))
}

pub(super) fn render_core_grid(
    usages: &[f32],
    histories: &[Vec<[f64; 2]>],
    theme: &gpui_component::theme::Theme,
) -> AnyElement {
    if usages.is_empty() {
        return empty_state("暂时没有 CPU 核心数据", theme).into_any_element();
    }

    let (columns, rows) = core_grid_dimensions(usages.len());
    let compact = usages.len() > 32;
    let columns = u16::try_from(columns).unwrap_or(u16::MAX);
    let rows = u16::try_from(rows).unwrap_or(u16::MAX);
    let mut grid = div()
        .debug_selector(|| "system-core-grid".to_owned())
        .grid()
        .grid_cols(columns)
        .grid_rows(rows)
        .w_full()
        .h_full()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .gap(px(if compact { 2.0 } else { 6.0 }));

    for (index, usage) in usages.iter().copied().enumerate() {
        grid = grid.child(render_core_tile(
            index,
            usage,
            histories.get(index).map(Vec::as_slice).unwrap_or(&[]),
            compact,
            theme,
        ));
    }

    grid.into_any_element()
}

fn render_core_tile(
    index: usize,
    usage: f32,
    history: &[[f64; 2]],
    compact: bool,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    let usage = usage.clamp(0.0, 100.0);
    let label = if compact {
        format!("{}", index + 1)
    } else {
        format!("核心 {}", index + 1)
    };
    let detail = if compact {
        None
    } else {
        Some(format_percent(usage as f64))
    };

    let header = h_flex().w_full().flex_none().min_w_0().gap(px(3.0)).child(
        div()
            .min_w_0()
            .flex_1()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_xs()
            .child(label),
    );
    let mut tile = v_flex()
        .debug_selector(|| format!("system-core-tile-{}", index + 1))
        .flex_1()
        .h_full()
        .min_w_0()
        .min_h_0()
        .gap(px(if compact { 0.0 } else { 3.0 }))
        .p(px(if compact { 2.0 } else { 5.0 }))
        .overflow_hidden()
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .rounded(px(3.0));

    if compact {
        // 核心很多时把编号叠放在图表上方，避免标题行挤掉图表高度。
        tile = tile.child(
            v_flex()
                .relative()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(render_core_history(history, usage, true, theme))
                .child(
                    div()
                        .absolute()
                        .top(px(1.0))
                        .left(px(1.0))
                        .px(px(1.0))
                        .text_size(px(9.0))
                        .line_height(px(10.0))
                        .text_color(theme.foreground)
                        .bg(theme.background)
                        .child((index + 1).to_string()),
                ),
        );
    } else {
        let header = if let Some(detail) = detail {
            header.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_nowrap()
                    .child(detail),
            )
        } else {
            header
        };
        tile = tile
            .child(header)
            .child(render_core_history(history, usage, false, theme));
    }

    tile.w_full().h_full().min_w_0().min_h_0()
}

fn render_core_history(
    history: &[[f64; 2]],
    usage: f32,
    compact: bool,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    let mut samples = history
        .iter()
        .rev()
        .take(if compact { 8 } else { 24 })
        .map(|point| point[1] as f32)
        .collect::<Vec<_>>();
    if samples.is_empty() {
        samples.push(usage);
    }

    let mut graph = h_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .items_end()
        .gap(px(1.0))
        .px(px(2.0))
        .py(px(2.0))
        .overflow_hidden()
        .bg(theme.muted);
    for sample in samples.iter().rev() {
        let height = (sample.clamp(0.0, 100.0) / 100.0).max(0.03);
        graph = graph.child(
            div()
                .flex_1()
                .min_w_0()
                .min_h(px(1.0))
                .h(relative(height))
                .bg(theme.accent),
        );
    }
    graph
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
        AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        Styled as _, TestAppContext, Window, div, px, size,
    };
    use gpui_component::{ActiveTheme as _, Root, v_flex};

    use super::{
        core_grid_dimensions, format_bytes, format_percent, format_rate_pair, history_max,
        ratio_percent, render_core_grid,
    };

    struct CoreGridTestHost;

    impl Render for CoreGridTestHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let usages = (0..128)
                .map(|index| (index % 100) as f32)
                .collect::<Vec<_>>();
            let histories = vec![Vec::new(); usages.len()];
            v_flex().size_full().child(
                v_flex()
                    .debug_selector(|| "system-core-panel".to_owned())
                    .w_full()
                    .h(px(260.0))
                    .gap(px(6.0))
                    .p(px(12.0))
                    .child(div().h(px(20.0)).flex_none())
                    .child(render_core_grid(&usages, &histories, cx.theme())),
            )
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

    #[test]
    fn core_grid_keeps_every_core_inside_the_fixed_window() {
        assert_eq!(core_grid_dimensions(0), (1, 1));
        assert_eq!(core_grid_dimensions(1), (1, 1));
        assert_eq!(core_grid_dimensions(12), (4, 3));
        assert_eq!(core_grid_dimensions(33), (9, 4));

        let (columns, rows) = core_grid_dimensions(128);
        assert_eq!((columns, rows), (16, 8));
        assert!(columns * rows >= 128);
    }

    #[gpui::test]
    fn core_grid_last_tile_is_not_clipped_by_the_fixed_window(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| CoreGridTestHost);
            Root::new(host, window, cx)
        });
        cx.simulate_resize(size(px(640.0), px(260.0)));
        cx.run_until_parked();

        let grid = cx
            .debug_bounds("system-core-grid")
            .expect("core grid should be rendered");
        let panel = cx
            .debug_bounds("system-core-panel")
            .expect("core panel should be rendered");
        let last_tile = cx
            .debug_bounds("system-core-tile-128")
            .expect("last core tile should be rendered");
        assert!(last_tile.size.width > px(0.0));
        assert!(last_tile.size.height > px(0.0));
        assert!(grid.origin.x >= panel.origin.x);
        assert!(grid.origin.y >= panel.origin.y);
        assert!(grid.origin.x + grid.size.width <= panel.origin.x + panel.size.width);
        assert!(grid.origin.y + grid.size.height <= panel.origin.y + panel.size.height);
        assert!(last_tile.origin.x >= panel.origin.x);
        assert!(last_tile.origin.y >= panel.origin.y);
        assert!(last_tile.origin.x + last_tile.size.width <= panel.origin.x + panel.size.width);
        assert!(last_tile.origin.y + last_tile.size.height <= panel.origin.y + panel.size.height);
        assert!(last_tile.origin.x + last_tile.size.width <= grid.origin.x + grid.size.width);
        assert!(last_tile.origin.y + last_tile.size.height <= grid.origin.y + grid.size.height);
    }
}
