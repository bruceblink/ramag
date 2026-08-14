//! 侧栏 Tag 行与操作菜单。

use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, px,
};
use gpui_component::{
    ActiveTheme, Icon, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::Tag;
use ramag_ui::PointerDropdownMenu as _;

use super::helpers::TagOp;
use super::sidebar::LEFT_ROW_H;
use super::vcs_view::VcsView;

pub(super) fn tag_row(
    idx: usize,
    t: &Tag,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let fg = theme.foreground;
    let muted_fg = theme.muted_foreground;
    let mono = theme.mono_font_family.clone();
    let hover_bg = theme.muted;
    let tag_color = gpui::hsla(40.0 / 360.0, 0.7, 0.55, 1.0);

    // 无说明时显示提交哈希。
    let detail = match &t.message {
        Some(m) => m.clone(),
        None => t.commit.short().to_string(),
    };
    let name = t.name.clone();
    let row_id = SharedString::from(format!("vcs-side-tag-{idx}-{name}"));

    let entity = cx.entity();
    let mut row = h_flex()
        .id(row_id)
        .h(px(LEFT_ROW_H))
        .flex_none()
        .gap(px(6.0))
        .items_center()
        .px(px(4.0))
        .rounded(px(3.0))
        .hover(move |this| this.bg(hover_bg))
        .child(
            div().flex_none().w(px(14.0)).child(
                Icon::new(ramag_ui::icons::circle_dot())
                    .xsmall()
                    .text_color(tag_color),
            ),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .items_baseline()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(super::inline_text_preview(&name, 120)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_xs()
                        .font_family(mono)
                        .text_color(muted_fg)
                        .child(super::inline_text_preview(&detail, 240)),
                ),
        )
        .cursor_pointer();
    let menu_entity = entity.clone();
    let menu_name = name.clone();
    row = row.child(
        ramag_ui::clickable_button(SharedString::from(format!("vcs-side-tag-more-{idx}")))
            .ghost()
            .xsmall()
            .icon(ramag_ui::icons::ellipsis())
            .tooltip("标签")
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| {
                tag_actions_menu(menu, menu_entity.clone(), menu_name.clone(), busy)
            }),
    );
    row.context_menu(move |menu: PopupMenu, _, _| {
        tag_actions_menu(menu, entity.clone(), name.clone(), busy)
    })
}

fn tag_actions_menu(
    mut menu: PopupMenu,
    entity: Entity<VcsView>,
    name: String,
    busy: bool,
) -> PopupMenu {
    let push_entity = entity.clone();
    let push_name = name.clone();
    menu = menu.item(ramag_ui::menu_item_with_disabled("推送", busy).on_click(
        move |_, window, app| {
            push_entity.update(app, |this, cx| {
                this.confirm_tag_op(TagOp::Push(push_name.clone()), window, cx);
            });
        },
    ));
    menu.item(
        ramag_ui::menu_item_with_disabled("删除", busy).on_click(move |_, window, app| {
            entity.update(app, |this, cx| {
                this.confirm_tag_op(TagOp::Delete(name.clone()), window, cx);
            });
        }),
    )
}
