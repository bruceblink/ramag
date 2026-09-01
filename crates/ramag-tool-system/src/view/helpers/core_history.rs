use gpui::{
    InteractiveElement, IntoElement, ParentElement, PathBuilder, Styled, canvas, fill, point, px,
    size,
};
use gpui_component::{theme::Theme, v_flex};

const CORE_HISTORY_POINTS: usize = 24;
const CORE_HISTORY_COMPACT_POINTS: usize = 12;
const CORE_HISTORY_HORIZONTAL_GRID_LINES: usize = 2;
const CORE_HISTORY_VERTICAL_GRID_LINES: usize = 3;

/// 将单个核心的历史采样绘制成占满卡片剩余空间的迷你折线图。
pub(super) fn render_core_history(
    index: usize,
    history: &[[f64; 2]],
    usage: f32,
    compact: bool,
    theme: &Theme,
) -> impl IntoElement {
    let points = core_history_points(
        history,
        usage,
        if compact {
            CORE_HISTORY_COMPACT_POINTS
        } else {
            CORE_HISTORY_POINTS
        },
    );
    let chart_background = theme.muted;
    let grid_color = theme.border.opacity(0.35);
    let accent = theme.accent;
    let line_width = if compact { 1.2 } else { 1.5 };
    let chart = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let padding = px(if compact { 1.0 } else { 2.0 });
            let chart_origin = bounds.origin + point(padding, padding);
            let chart_width = (bounds.size.width - padding * 2.0).max(px(1.0));
            let chart_height = (bounds.size.height - padding * 2.0).max(px(1.0));
            let chart_bounds = gpui::Bounds::new(chart_origin, size(chart_width, chart_height));
            window.paint_quad(fill(chart_bounds, chart_background));

            let mut grid = PathBuilder::stroke(px(1.0));
            for grid_index in 1..=CORE_HISTORY_HORIZONTAL_GRID_LINES {
                let fraction = grid_index as f32 / (CORE_HISTORY_HORIZONTAL_GRID_LINES + 1) as f32;
                let y = chart_origin.y + chart_height * fraction;
                grid.move_to(point(chart_origin.x, y));
                grid.line_to(point(chart_origin.x + chart_width, y));
            }
            for grid_index in 1..=CORE_HISTORY_VERTICAL_GRID_LINES {
                let fraction = grid_index as f32 / (CORE_HISTORY_VERTICAL_GRID_LINES + 1) as f32;
                let x = chart_origin.x + chart_width * fraction;
                grid.move_to(point(x, chart_origin.y));
                grid.line_to(point(x, chart_origin.y + chart_height));
            }
            if let Ok(path) = grid.build() {
                window.paint_path(path, grid_color);
            }

            let coordinates = points
                .iter()
                .enumerate()
                .map(|(point_index, sample)| {
                    let x_fraction = if points.len() == 1 {
                        0.5
                    } else {
                        point_index as f32 / (points.len() - 1) as f32
                    };
                    let value_fraction = core_chart_value_ratio(sample[1]);
                    point(
                        chart_origin.x + chart_width * x_fraction,
                        chart_origin.y + chart_height * (1.0 - value_fraction),
                    )
                })
                .collect::<Vec<_>>();

            if coordinates.len() > 1 {
                let mut area = PathBuilder::fill();
                area.move_to(point(coordinates[0].x, chart_origin.y + chart_height));
                for coordinate in &coordinates {
                    area.line_to(*coordinate);
                }
                area.line_to(point(
                    coordinates[coordinates.len() - 1].x,
                    chart_origin.y + chart_height,
                ));
                area.close();
                if let Ok(path) = area.build() {
                    window.paint_path(path, accent.opacity(0.16));
                }
            }

            let mut line = PathBuilder::stroke(px(line_width));
            line.move_to(coordinates[0]);
            if coordinates.len() == 1 {
                line.line_to(point(coordinates[0].x + px(2.0), coordinates[0].y));
            } else {
                for coordinate in coordinates.iter().skip(1) {
                    line.line_to(*coordinate);
                }
            }
            if let Ok(path) = line.build() {
                window.paint_path(path, accent);
            }
        },
    );

    v_flex()
        .debug_selector(|| format!("system-core-line-chart-{}", index + 1))
        .flex_1()
        .h_full()
        .min_w_0()
        .min_h_0()
        .w_full()
        .overflow_hidden()
        .child(chart.flex_1().min_w_0().min_h_0())
}

pub(super) fn core_history_points(
    history: &[[f64; 2]],
    usage: f32,
    max_points: usize,
) -> Vec<[f64; 2]> {
    let start = history.len().saturating_sub(max_points.max(1));
    let mut points = history[start..]
        .iter()
        .map(|sample| [sample[0], sanitize_core_usage(sample[1])])
        .collect::<Vec<_>>();
    if points.is_empty() {
        points.push([0.0, sanitize_core_usage(f64::from(usage))]);
    }
    points
}

fn sanitize_core_usage(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

pub(super) fn core_chart_value_ratio(value: f64) -> f32 {
    (sanitize_core_usage(value) / 100.0) as f32
}
