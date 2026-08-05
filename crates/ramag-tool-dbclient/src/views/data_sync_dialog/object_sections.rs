use gpui::{Anchor, ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, h_flex, input::Input,
    scroll::ScrollableElement as _, v_flex,
};
use ramag_ui::PointerDropdownMenu as _;

use super::catalog::visible_catalog_items;
use super::{DataSyncDialog, value};
use crate::views::inline_text_preview;

const OBJECT_ROW_HEIGHT: f32 = 36.0;
const MAX_VISIBLE_OBJECT_ROWS: usize = 5;

fn object_list_height(item_count: usize) -> f32 {
    item_count.clamp(1, MAX_VISIBLE_OBJECT_ROWS) as f32 * OBJECT_ROW_HEIGHT
}

impl DataSyncDialog {
    pub(super) fn render_object_selector(
        &self,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (visible, matched) = self.visible_source_objects(cx);
        let shown = visible.len();
        let selected_count = self.mapping_editors.len();
        let all_visible_selected = shown > 0
            && visible.iter().all(|object| {
                self.mapping_editors
                    .iter()
                    .any(|mapping| mapping.source == *object)
            });
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mut selected_background = cx.theme().accent;
        selected_background.a = 0.06;
        let mut rows = v_flex().w_full();
        for (visible_index, object) in visible.into_iter().enumerate() {
            let selected = self
                .mapping_editors
                .iter()
                .any(|mapping| mapping.source == object);
            let id = gpui::SharedString::from(format!(
                "sync-source-object-{}-{visible_index}",
                self.catalog_generation
            ));
            let object_for_action = object.clone();
            let entity = cx.entity();
            let target_editor = self
                .mapping_editors
                .iter()
                .enumerate()
                .find(|(_, mapping)| mapping.source == object)
                .map(|(mapping_index, mapping)| {
                    let target_input = mapping.target.clone();
                    let target_query = target_input.read(cx).value().to_string();
                    let (candidates, matched) =
                        visible_catalog_items(&self.target_objects, &target_query);
                    let current = value(&target_input, cx);
                    let input_for_menu = target_input.clone();
                    let picker = ramag_ui::clickable_button(gpui::SharedString::from(format!(
                        "sync-target-object-picker-{mapping_index}"
                    )))
                    .outline()
                    .small()
                    .label(format!("已有（{matched}）"))
                    .dropdown_caret(true)
                    .disabled(busy || candidates.is_empty())
                    .pointer_dropdown_menu_with_anchor(
                        Anchor::BottomLeft,
                        move |mut menu, _, _| {
                            menu = menu
                                .scrollable(true)
                                .max_h(px(super::DROPDOWN_MENU_MAX_HEIGHT));
                            for candidate in &candidates {
                                let candidate_for_action = candidate.clone();
                                let input = input_for_menu.clone();
                                menu = menu.item(
                                    ramag_ui::menu_item(candidate.clone())
                                        .checked(current == *candidate)
                                        .on_click(move |_: &ClickEvent, window, app| {
                                            input.update(app, |input, cx| {
                                                input.set_value(&candidate_for_action, window, cx);
                                            });
                                        }),
                                );
                            }
                            menu
                        },
                    );
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(div().text_sm().child("→"))
                        .child(Input::new(&target_input).disabled(busy).flex_1())
                        .when(!self.target_objects.is_empty(), |editor| {
                            editor.child(picker)
                        })
                        .into_any_element()
                });
            let has_target_editor = target_editor.is_some();
            rows = rows.child(
                h_flex()
                    .w_full()
                    .h(px(OBJECT_ROW_HEIGHT))
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(9.0))
                    .border_b_1()
                    .border_color(border)
                    .when(selected, |row| row.bg(selected_background))
                    .child(
                        ramag_ui::clickable_checkbox(id)
                            .checked(selected)
                            .disabled(busy)
                            .on_click(move |_: &bool, window, app| {
                                entity.update(app, |this, cx| {
                                    this.toggle_mapping(object_for_action.clone(), window, cx);
                                });
                            }),
                    )
                    .child(
                        div()
                            .when(has_target_editor, |name| name.w(px(210.0)).flex_none())
                            .when(!has_target_editor, |name| name.flex_1())
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(inline_text_preview(&object, 96)),
                    )
                    .when_some(target_editor, |row, editor| row.child(editor)),
            );
        }

        v_flex()
            .id("data-sync-object-selector")
            .w_full()
            .gap(px(8.0))
            .p(px(10.0))
            .border_1()
            .border_color(border)
            .rounded(px(6.0))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("选择对象"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("共 {matched} · 已选 {selected_count}")),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(px(8.0))
                    .child(Input::new(&self.object_query).disabled(busy).flex_1())
                    .child(
                        ramag_ui::clickable_button("sync-select-visible")
                            .outline()
                            .small()
                            .label(if all_visible_selected {
                                "清空"
                            } else {
                                "全选"
                            })
                            .disabled(busy || shown == 0)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_visible_mappings(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .h(px(object_list_height(shown)))
                    .overflow_y_scrollbar()
                    .border_1()
                    .border_color(border)
                    .rounded(px(5.0))
                    .child(rows),
            )
            .when(matched > shown, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("仅展示前 {shown} 个，请搜索缩小范围。")),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_list_height_is_bounded_and_scrollable() {
        assert_eq!(object_list_height(0), 36.0);
        assert_eq!(object_list_height(1), 36.0);
        assert_eq!(object_list_height(4), 144.0);
        assert_eq!(object_list_height(5), 180.0);
        assert_eq!(object_list_height(8), 180.0);
        assert_eq!(object_list_height(25), 180.0);
    }
}
