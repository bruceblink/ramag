//! 结果表的空结果和搜索阻塞状态。

use gpui::{AnyElement, ParentElement, Styled, div, prelude::*};
use gpui_component::v_flex;

use super::super::result_panel::RowSearchBlocker;

pub(super) fn render_affected_result(
    affected: u64,
    elapsed: u64,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_lg()
                .text_color(fg)
                .child(format!("✓ {affected} 行受影响")),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted_fg)
                .child(format!("{elapsed} ms")),
        )
        .into_any_element()
}

pub(super) fn render_row_search_blocker(
    blocker: RowSearchBlocker,
    muted_fg: gpui::Hsla,
    danger: gpui::Hsla,
) -> AnyElement {
    let (message, color) = match blocker {
        RowSearchBlocker::Converting => ("正在通过外部程序转换 ID…".to_string(), muted_fg),
        RowSearchBlocker::Error(error) => (format!("ID 转换失败：{error}"), danger),
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .px_4()
        .text_xs()
        .text_color(color)
        .child(message)
        .child(
            div()
                .text_color(muted_fg)
                .child("请修改搜索词，或检查设置中的转换方式。"),
        )
        .into_any_element()
}
