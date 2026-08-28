//! 结果表分页大小选择器。

use gpui::IntoElement;
use gpui_component::button::ButtonVariants as _;
use gpui_component::{Disableable as _, Sizable as _};
use ramag_ui::PointerDropdownMenu as _;

use crate::views::result_panel::{ResultPanel, ResultPanelEvent};

/// 构建页大小菜单，并在发出事件前限制自定义输入范围。
pub(in crate::views) fn render_page_size_selector(
    current: usize,
    panel: gpui::Entity<ResultPanel>,
    disabled: bool,
) -> impl IntoElement {
    let menu_panel = panel.clone();
    ramag_ui::clickable_button("result-page-size")
        .ghost()
        .small()
        .label(format!("每页 {current} 行"))
        .dropdown_caret(true)
        .disabled(disabled)
        .pointer_dropdown_menu(move |mut menu, _, _| {
            for size in ramag_ui::RESULT_PAGE_SIZE_PRESETS {
                let panel = menu_panel.clone();
                menu = menu.item(
                    ramag_ui::menu_item(format!("每页 {size} 行"))
                        .checked(size == current)
                        .on_click(move |_, _, app| {
                            panel.update(app, |_, cx| {
                                cx.emit(ResultPanelEvent::PageSizeChanged(size));
                            });
                        }),
                );
            }
            let panel = menu_panel.clone();
            menu.separator().item(
                ramag_ui::menu_item("自定义…")
                    .checked(!ramag_ui::RESULT_PAGE_SIZE_PRESETS.contains(&current))
                    .on_click(move |_, window, app| {
                        let panel = panel.clone();
                        ramag_ui::open_bounded_prompt(
                            "自定义每页行数",
                            "输入 1-10000 的整数",
                            &current.to_string(),
                            "应用",
                            16,
                            move |value, _, app| match ramag_ui::parse_result_page_size(&value) {
                                Ok(size) => panel.update(app, |_, cx| {
                                    cx.emit(ResultPanelEvent::PageSizeChanged(size));
                                }),
                                Err(message) => panel.update(app, |panel, cx| {
                                    panel.notify_page_size_error(message, cx);
                                }),
                            },
                            window,
                            app,
                        );
                    }),
            )
        })
}
