use gpui::{Anchor, ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex, input::Input,
    scroll::ScrollableElement as _, v_flex,
};
use ramag_ui::PointerDropdownMenu as _;

use super::catalog::visible_catalog_items;
use super::{DataSyncDialog, value};
use crate::views::inline_text_preview;

impl DataSyncDialog {
    pub(super) fn render_object_selector(
        &self,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (visible, matched) = self.visible_source_objects(cx);
        let shown = visible.len();
        let selected_count = self.mapping_editors.len();
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mut chips = h_flex().w_full().flex_wrap().gap(px(6.0));
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
            chips = chips.child(
                ramag_ui::clickable_button(id)
                    .small()
                    .label(inline_text_preview(&object, 64))
                    .disabled(busy)
                    .when(selected, |button| button.primary())
                    .when(!selected, |button| button.outline())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.toggle_mapping(object_for_action.clone(), window, cx);
                    })),
            );
        }

        v_flex()
            .w_full()
            .gap(px(8.0))
            .p(px(10.0))
            .border_1()
            .border_color(border)
            .rounded(px(6.0))
            .child(
                h_flex()
                    .w_full()
                    .gap(px(8.0))
                    .child(Input::new(&self.object_query).disabled(busy).flex_1())
                    .child(
                        ramag_ui::clickable_button("sync-select-visible")
                            .outline()
                            .small()
                            .label("全选当前结果")
                            .disabled(busy || shown == 0)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.select_visible_mappings(window, cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("sync-clear-selected")
                            .ghost()
                            .small()
                            .label("清空")
                            .disabled(busy || selected_count == 0)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.clear_mappings(cx);
                            })),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(format!(
                "匹配 {matched} 个，当前展示 {shown} 个，已选择 {selected_count} 个"
            )))
            .child(
                div()
                    .w_full()
                    .max_h(px(170.0))
                    .overflow_y_scrollbar()
                    .child(chips),
            )
            .when(matched > shown, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("结果较多，请输入关键词继续缩小范围。"),
                )
            })
    }

    pub(super) fn render_mapping_editors(
        &self,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut rows = v_flex().w_full().gap(px(6.0));
        for (index, mapping) in self.mapping_editors.iter().enumerate() {
            let source = mapping.source.clone();
            let target_input = mapping.target.clone();
            let target_query = target_input.read(cx).value().to_string();
            let (candidates, matched) = visible_catalog_items(&self.target_objects, &target_query);
            let current = value(&target_input, cx);
            let picker = ramag_ui::clickable_button(gpui::SharedString::from(format!(
                "sync-target-object-picker-{index}"
            )))
            .outline()
            .small()
            .label(if self.target_objects.is_empty() {
                "无已有对象".to_string()
            } else {
                format!("匹配已有 {matched}")
            })
            .dropdown_caret(true)
            .disabled(busy || candidates.is_empty())
            .pointer_dropdown_menu_with_anchor(
                Anchor::BottomLeft,
                move |mut menu, _, _| {
                    for candidate in &candidates {
                        let candidate_for_action = candidate.clone();
                        let input = target_input.clone();
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
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap(px(8.0))
                    .child(
                        div()
                            .w(px(210.0))
                            .text_sm()
                            .child(inline_text_preview(&source, 64)),
                    )
                    .child(div().text_sm().child("→"))
                    .child(Input::new(&mapping.target).disabled(busy).flex_1())
                    .child(picker),
            );
        }
        v_flex()
            .w_full()
            .gap(px(6.0))
            .when(!self.mapping_editors.is_empty(), |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("目标名称映射（可选择已有对象或输入新名称）"),
                )
            })
            .child(
                div()
                    .w_full()
                    .max_h(px(220.0))
                    .overflow_y_scrollbar()
                    .child(rows),
            )
    }
}
