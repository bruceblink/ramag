//! 查询结果差异比较的工具栏菜单。

use gpui::{ClickEvent, Entity, IntoElement};
use gpui_component::{Disableable as _, Sizable as _, button::ButtonVariants as _};
use ramag_ui::PointerDropdownMenu as _;

use super::QueryTab;
use crate::views::result_panel::ResultPanel;

pub(super) fn result_comparison_menu(
    query_tab: Entity<QueryTab>,
    result: Entity<ResultPanel>,
    can_capture: bool,
    has_baseline: bool,
    can_cross_compare: bool,
) -> impl IntoElement {
    let disabled = !can_capture && !has_baseline && !can_cross_compare;
    ramag_ui::clickable_button("result-comparison-menu")
        .ghost()
        .small()
        .icon(ramag_ui::icons::git_compare())
        .label("比较")
        .dropdown_caret(true)
        .tooltip(if has_baseline {
            "管理当前查询结果的比较基准"
        } else {
            "保存当前结果并比较重新执行后的变化"
        })
        .disabled(disabled)
        .pointer_dropdown_menu(move |mut menu, _, _| {
            let result_for_capture = result.clone();
            menu = menu.item(
                ramag_ui::menu_item_with_disabled("保存当前结果为基准", !can_capture).on_click(
                    move |_: &ClickEvent, _, app| {
                        result_for_capture.update(app, |panel, cx| {
                            panel.capture_comparison_baseline(cx);
                        });
                    },
                ),
            );

            let result_for_compare = result.clone();
            menu = menu.item(
                ramag_ui::menu_item_with_disabled(
                    "比较当前结果与基准",
                    !can_capture || !has_baseline,
                )
                .on_click(move |_: &ClickEvent, window, app| {
                    result_for_compare.update(app, |panel, cx| {
                        panel.open_comparison_dialog(window, cx);
                    });
                }),
            );

            let query_tab_for_cross = query_tab.clone();
            menu = menu.item(
                ramag_ui::menu_item_with_disabled("与其他连接比较结果", !can_cross_compare)
                    .on_click(move |_: &ClickEvent, window, app| {
                        query_tab_for_cross.update(app, |tab, cx| {
                            tab.prompt_cross_connection_result_compare(window, cx);
                        });
                    }),
            );

            menu = menu.separator();
            let result_for_clear = result.clone();
            menu.item(
                ramag_ui::menu_item_with_disabled("清除对比基准", !has_baseline).on_click(
                    move |_: &ClickEvent, _, app| {
                        result_for_clear.update(app, |panel, cx| {
                            panel.clear_comparison_baseline(cx);
                        });
                    },
                ),
            )
        })
}
