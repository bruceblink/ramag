use gpui::{ClickEvent, Entity, IntoElement, ParentElement, Styled, div, prelude::*};
use gpui_component::{
    IconName, Sizable as _, button::ButtonVariants as _, h_flex, input::InputState,
};
use ramag_ui::PointerDropdownMenu as _;

use crate::views::result_panel::{ResultPanel, RowSearchConversionStatus, RowSearchMode};

pub(super) fn row_filter_prefix(
    current: RowSearchMode,
    result: Entity<ResultPanel>,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    id_conversion_ready: bool,
) -> gpui::AnyElement {
    if id_conversion_ready {
        row_search_mode_button(current, result, accent).into_any_element()
    } else {
        div()
            .flex_none()
            .text_xs()
            .text_color(muted)
            .child("WHERE")
            .into_any_element()
    }
}

pub(super) fn row_search_mode_button(
    current: RowSearchMode,
    result: Entity<ResultPanel>,
    accent: gpui::Hsla,
) -> impl IntoElement {
    let display_label = match current {
        RowSearchMode::Normal => "WHERE",
        RowSearchMode::IdToInteger => current.label(),
        RowSearchMode::IdToString => current.label(),
    };
    ramag_ui::clickable_button("sql-row-search-mode")
        .text()
        .small()
        // 文本自带显式颜色，避免 Text 按下态短暂继承主题前景色。
        .child(div().flex_none().text_color(accent).child(display_label))
        .dropdown_caret(true)
        .text_color(accent)
        .tooltip(match current {
            RowSearchMode::Normal => "WHERE：按 Enter 将条件发送到数据库执行",
            RowSearchMode::IdToInteger => "@ID -> I：将字符串转为整数，精确匹配整数单元格",
            RowSearchMode::IdToString => "@ID -> S：将非负十进制整数转为字符串，精确匹配文本单元格",
        })
        .pointer_dropdown_menu(move |mut menu, _, _| {
            for mode in RowSearchMode::ALL {
                let result = result.clone();
                menu = menu.item(
                    ramag_ui::menu_item(mode.label())
                        .checked(mode == current)
                        .on_click(move |_: &ClickEvent, _, app| {
                            result.update(app, |panel, cx| {
                                panel.set_row_search_mode(mode, cx);
                            });
                        }),
                );
            }
            menu
        })
}

pub(super) fn row_search_input_suffix(
    input: Entity<InputState>,
    status: Option<RowSearchConversionStatus>,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .when_some(status, |suffix, status| {
            suffix.child(row_search_conversion_label(status, accent, muted, danger))
        })
        .child(
            ramag_ui::clickable_button("sql-row-filter-clear")
                .icon(IconName::CircleX)
                .ghost()
                .xsmall()
                .tab_stop(false)
                .text_color(muted)
                .tooltip("清除")
                .on_click(move |_, window, cx| {
                    input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                }),
        )
}

fn row_search_conversion_label(
    status: RowSearchConversionStatus,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> gpui::AnyElement {
    let (label, color) = match status {
        RowSearchConversionStatus::Converting => ("→ 转换中…".to_string(), muted),
        RowSearchConversionStatus::Ready(output) => {
            (format!("→ {}", output.display_preview(40)), accent)
        }
        RowSearchConversionStatus::Error(_) => ("→ 转换失败".to_string(), danger),
    };

    div()
        .flex_none()
        .text_xs()
        .text_color(color)
        .child(label)
        .into_any_element()
}
