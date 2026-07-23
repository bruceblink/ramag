//! 侧栏：分支行（名字 + 上游同步，操作走右键菜单）+ 本地段底部「新建分支」输入行。
//! 行由 history 左栏的单个 uniform_list 统一渲染（28px 等高），段组装见 history_panel

use gpui::{
    AnyElement, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::Input,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::Branch;
use ramag_ui::PointerDropdownMenu as _;

use super::confirm_dialogs::open_confirm_dialog;
use super::helpers::{BranchOp, checkout_remote_branch_op};
use super::sidebar::LEFT_ROW_H;
use super::vcs_view::VcsView;

impl VcsView {
    /// 本地段底部「新建分支」输入行（name 输入 + 创建按钮，固定 28px 高）
    pub(super) fn render_create_branch_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy;
        let has_head = self
            .status
            .as_ref()
            .and_then(|status| status.head_commit.as_ref())
            .is_some();
        h_flex()
            .h(px(LEFT_ROW_H))
            .flex_none()
            .gap(px(4.0))
            .items_center()
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.create_branch_input)
                        .xsmall()
                        .into_any_element(),
                ),
            )
            .child(
                ramag_ui::clickable_button("vcs-branch-create")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .when(!has_head, |button| button.tooltip("请先提交"))
                    .disabled(busy || !has_head)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.handle_create_branch(cx);
                    })),
            )
            .into_any_element()
    }
}

/// 单条分支行：图标 + 名字 + 上游同步；操作通过右键菜单触发（固定 28px 高）
pub(super) fn branch_row(
    idx: usize,
    b: &Branch,
    busy: bool,
    is_remote: bool,
    cx: &mut Context<VcsView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let fg = theme.foreground;
    let muted_fg = theme.muted_foreground;
    let accent = theme.accent;
    let hover_bg = theme.muted;
    let entity = cx.entity();

    let name = b.name.clone();
    let is_head = b.is_head;
    let name_color = if is_head { accent } else { fg };
    let prefix_color = if is_head { accent } else { muted_fg };

    let sync_str = match (b.ahead, b.behind) {
        (Some(a), Some(d)) if a > 0 || d > 0 => Some(format!("↑{a} ↓{d}")),
        _ => None,
    };

    let row_id = SharedString::from(format!("vcs-side-br-{}-{}-{}", idx, is_remote, name));
    let prefix_icon = if is_head {
        Icon::new(ramag_ui::icons::circle_dot())
            .xsmall()
            .text_color(prefix_color)
            .into_any_element()
    } else {
        Icon::new(ramag_ui::icons::git_branch())
            .xsmall()
            .text_color(prefix_color)
            .into_any_element()
    };

    let mut row = h_flex()
        .id(row_id)
        .h(px(LEFT_ROW_H))
        .flex_none()
        .gap(px(6.0))
        .items_center()
        .px(px(4.0))
        .rounded(px(3.0))
        .hover(move |this| this.bg(hover_bg))
        .child(div().flex_none().w(px(14.0)).child(prefix_icon))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(if is_head {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(name_color)
                .overflow_hidden()
                .text_ellipsis()
                .child(super::inline_text_preview(&name, 160)),
        )
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(muted_fg)
                .child(sync_str.unwrap_or_default()),
        );

    // 当前 HEAD 分支没有可用操作：不挂菜单，避免右键弹出空菜单
    if is_head {
        return row.into_any_element();
    }
    row = row.cursor_pointer();

    // 行尾「⋯」：与右键菜单同一份操作，给不习惯右键的用户一个可见入口
    let more_btn = {
        let ent = entity.clone();
        let n = name.clone();
        ramag_ui::clickable_button(SharedString::from(format!(
            "vcs-side-br-more-{idx}-{is_remote}"
        )))
        .ghost()
        .xsmall()
        .icon(ramag_ui::icons::ellipsis())
        .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| {
            branch_actions_menu(menu, ent.clone(), n.clone(), is_remote, busy)
        })
    };
    row = row.child(div().flex_none().child(more_btn));

    // 右键菜单：checkout / merge / rebase / interactive-rebase / delete
    row.context_menu({
        let ent = entity.clone();
        let n = name.clone();
        move |menu: PopupMenu, _, _| {
            branch_actions_menu(menu, ent.clone(), n.clone(), is_remote, busy)
        }
    })
    .into_any_element()
}

fn branch_actions_menu(
    menu: PopupMenu,
    ent: Entity<VcsView>,
    n: String,
    is_remote: bool,
    busy: bool,
) -> PopupMenu {
    let (e1, n1) = (ent.clone(), n.clone());
    let (e2, n2) = (ent.clone(), n.clone());
    let (e3, n3) = (ent.clone(), n.clone());
    let n4 = n.clone();
    let mut m = menu;
    if !is_remote {
        m = m.item(ramag_ui::menu_item("切换").on_click(move |_, w, app| {
            e1.update(app, |this, cx| {
                this.confirm_branch_op(BranchOp::Checkout(n1.clone()), w, cx);
            });
        }));
    } else {
        m = m.item(ramag_ui::menu_item("检出").on_click(move |_, w, app| {
            e1.update(app, |this, cx| {
                let op = match checkout_remote_branch_op(&n1, &this.local_branches) {
                    Ok(op) => op,
                    Err(message) => {
                        this.error = Some(message);
                        cx.notify();
                        return;
                    }
                };
                this.confirm_branch_op(op, w, cx);
            });
        }));
    }
    m = m.item(ramag_ui::menu_item("合并").on_click(move |_, w, app| {
        e2.update(app, |this, cx| {
            this.confirm_branch_op(BranchOp::Merge(n2.clone()), w, cx);
        });
    }));
    m = m.item(ramag_ui::menu_item("变基").on_click(move |_, w, app| {
        e3.update(app, |this, cx| {
            this.confirm_branch_op(BranchOp::Rebase(n3.clone()), w, cx);
        });
    }));
    if !is_remote {
        let (ei, ni) = (ent.clone(), n.clone());
        m = m.item(ramag_ui::menu_item("交互变基").on_click(move |_, _, app| {
            if !busy {
                ei.update(app, |this, cx| {
                    this.start_interactive_rebase(ni.clone(), cx);
                });
            }
        }));
        m = m.separator();
        let ed = ent.clone();
        m = m.item(ramag_ui::menu_item("删除").on_click(move |_, w, app| {
            let view = ed.clone();
            let branch_name = n4.clone();
            open_confirm_dialog(
                view,
                "删除分支？",
                format!(
                    "将删除本地分支「{branch_name}」（仅当已合并；未合并会报错）。\n确认继续吗？"
                ),
                "删除",
                true,
                move |this, cx| {
                    this.run_branch_op(BranchOp::Delete(branch_name.clone(), false), cx)
                },
                w,
                app,
            );
        }));
    }
    m
}
