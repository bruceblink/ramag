use gpui::{InteractiveElement, IntoElement, ParentElement, Styled, div, px, relative};
use gpui_component::{h_flex, theme::Theme};

pub(super) const CORE_HISTORY_COMPACT_HEIGHT: f32 = 12.0;
pub(super) const CORE_HISTORY_HEIGHT: f32 = 8.0;
pub(super) const CORE_HISTORY_MIN_BAR_HEIGHT: f32 = 2.0;
pub(super) const CORE_HISTORY_MIN_BAR_RATIO: f32 = 0.03;

/// 将核心采样绘制成固定高度的柱状图，让低占用核心也保留可见基线。
pub(super) fn render_core_history(
    index: usize,
    history: &[[f64; 2]],
    usage: f32,
    compact: bool,
    theme: &Theme,
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

    let graph_height = if compact {
        CORE_HISTORY_COMPACT_HEIGHT
    } else {
        CORE_HISTORY_HEIGHT
    };
    let mut graph = h_flex()
        .debug_selector(|| format!("system-core-history-{}", index + 1))
        .flex_none()
        .h(px(graph_height))
        .min_h_0()
        .w_full()
        .items_end()
        .gap(px(1.0))
        .px(px(2.0))
        .py(px(2.0))
        .overflow_hidden()
        .bg(theme.muted);
    for (sample_index, sample) in samples.iter().rev().enumerate() {
        graph = graph.child(
            div()
                .flex_1()
                .min_w_0()
                .debug_selector(|| format!("system-core-bar-{}-{}", index + 1, sample_index + 1))
                .min_h(px(CORE_HISTORY_MIN_BAR_HEIGHT))
                .h(relative(core_histogram_bar_ratio(*sample)))
                .rounded(px(1.0))
                .bg(theme.accent),
        );
    }
    graph
}

/// 计算单个核心柱条的相对高度；无效采样按零处理，并保留最低可见比例。
pub(super) fn core_histogram_bar_ratio(sample: f32) -> f32 {
    let sample = if sample.is_finite() {
        sample.clamp(0.0, 100.0)
    } else {
        0.0
    };
    (sample / 100.0).max(CORE_HISTORY_MIN_BAR_RATIO)
}
