//! 侧栏「远程仓库」：名称、fetch URL 与操作菜单。
//! 与「远程分支」区分：这里管 remote 配置（origin 等），非远端分支引用。
//! 行由 history 左栏的单个 uniform_list 统一渲染（28px 等高），段组装见 history_panel

use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::Remote;
use ramag_ui::PointerDropdownMenu as _;

use super::sidebar::LEFT_ROW_H;
use super::vcs_view::VcsView;

/// 单条 remote 行：[globe] name + fetch URL 内联 + 行尾 [改URL][重命名][删除]（固定 28px 高）
pub(super) fn remote_row(
    idx: usize,
    r: &Remote,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let fg = theme.foreground;
    let muted_fg = theme.muted_foreground;
    let mono = theme.mono_font_family.clone();
    let hover_bg = theme.muted;
    // 蓝青色：远程仓库与分支（暖橙 tag / 常规分支）区分
    let remote_color = gpui::hsla(200.0 / 360.0, 0.6, 0.55, 1.0);

    let name = r.name.clone();
    let url = r.fetch_url.clone();
    let row_id = SharedString::from(format!("vcs-side-remote-{idx}-{name}"));

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
            div()
                .flex_none()
                .w(px(14.0))
                .child(Icon::new(IconName::Globe).xsmall().text_color(remote_color)),
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
                        .child(super::inline_text_preview(&url, 240)),
                ),
        )
        .cursor_pointer();
    let menu_entity = entity.clone();
    let menu_name = name.clone();
    let menu_url = url.clone();
    row = row.child(
        ramag_ui::clickable_button(SharedString::from(format!("vcs-side-remote-more-{idx}")))
            .ghost()
            .xsmall()
            .icon(ramag_ui::icons::ellipsis())
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| {
                remote_actions_menu(
                    menu,
                    menu_entity.clone(),
                    menu_name.clone(),
                    menu_url.clone(),
                    busy,
                )
            }),
    );
    row.context_menu(move |menu: PopupMenu, _, _| {
        remote_actions_menu(menu, entity.clone(), name.clone(), url.clone(), busy)
    })
}

fn remote_actions_menu(
    mut menu: PopupMenu,
    entity: Entity<VcsView>,
    name: String,
    url: String,
    busy: bool,
) -> PopupMenu {
    let url_entity = entity.clone();
    let url_name = name.clone();
    menu = menu.item(
        ramag_ui::menu_item_with_disabled("修改地址", busy).on_click(move |_, window, app| {
            url_entity.update(app, |this, cx| {
                this.prompt_remote_set_url(url_name.clone(), url.clone(), window, cx);
            });
        }),
    );
    let rename_entity = entity.clone();
    let rename_name = name.clone();
    menu = menu.item(ramag_ui::menu_item_with_disabled("重命名", busy).on_click(
        move |_, window, app| {
            rename_entity.update(app, |this, cx| {
                this.prompt_remote_rename(rename_name.clone(), window, cx);
            });
        },
    ));
    menu.item(
        ramag_ui::menu_item_with_disabled("删除", busy).on_click(move |_, window, app| {
            entity.update(app, |this, cx| {
                this.confirm_remote_delete(name.clone(), window, cx);
            });
        }),
    )
}
