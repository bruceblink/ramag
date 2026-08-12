//! History / Graph 工具栏远端操作：快捷同步 + dropdown（Fetch / Pull / Push / 强推）

use gpui::{AnyElement, ClickEvent, Context, IntoElement, ParentElement as _, Styled as _, div};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _,
};

use super::helpers::RemoteOp;
use super::vcs_view::VcsView;
use ramag_ui::PointerDropdownMenu as _;

impl VcsView {
    /// 同步快捷主按钮：待拉取时显示「Pull ↓N」、待推送时显示「Push ↑N」，一键可达；
    /// 其余情形返回空（完整操作仍在聚合菜单里）
    pub(super) fn render_sync_quick_action(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.repo.is_none() {
            return div().into_any_element();
        }
        // 远端操作进行中：显示实时进度行 + 取消按钮（Fetch / Pull / Push 与 Clone 同款体验）
        if self.remote_op_cancel.is_some() {
            let line = self
                .remote_op_progress_line()
                .unwrap_or_else(|| self.busy_label.unwrap_or("处理中…").to_string());
            return gpui_component::h_flex()
                .items_center()
                .gap(gpui::px(6.0))
                .child(
                    div()
                        .max_w(gpui::px(240.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(line),
                )
                .child(
                    ramag_ui::clickable_button("vcs-remote-cancel")
                        .danger()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.cancel_remote_op(cx);
                        })),
                )
                .into_any_element();
        }
        let ahead = self.status.as_ref().and_then(|s| s.ahead).unwrap_or(0);
        let behind = self.status.as_ref().and_then(|s| s.behind).unwrap_or(0);
        let diverged = ahead > 0 && behind > 0;
        let (id, label, op): (&'static str, String, RemoteOp) = if diverged {
            (
                "vcs-quick-diverged",
                format!("分叉 ↑{ahead} ↓{behind}"),
                RemoteOp::Pull,
            )
        } else if behind > 0 {
            ("vcs-quick-pull", format!("拉取 ↓{behind}"), RemoteOp::Pull)
        } else if ahead > 0 {
            ("vcs-quick-push", format!("推送 ↑{ahead}"), RemoteOp::Push)
        } else {
            return div().into_any_element();
        };
        ramag_ui::clickable_button(id)
            .primary()
            .xsmall()
            .label(label)
            .disabled(self.busy)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.confirm_remote_op(op, window, cx);
            }))
            .into_any_element()
    }

    /// Git 操作聚合：dropdown（Fetch / Pull / Push / 强推）。Pull / Push 按 ahead/behind 显示数字
    pub(super) fn render_remote_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.repo.is_none() {
            return div().into_any_element();
        }
        let busy = self.busy;
        let ahead = self.status.as_ref().and_then(|s| s.ahead).unwrap_or(0);
        let behind = self.status.as_ref().and_then(|s| s.behind).unwrap_or(0);
        let has_head = self
            .status
            .as_ref()
            .and_then(|status| status.head_commit.as_ref())
            .is_some();
        let entity = cx.entity();

        let pull_label = if behind > 0 {
            format!("拉取 ↓{behind}")
        } else {
            "拉取".to_string()
        };
        let push_label = if ahead > 0 {
            format!("推送 ↑{ahead}")
        } else {
            "推送".to_string()
        };

        ramag_ui::clickable_button("vcs-ops-menu")
            .ghost()
            .xsmall()
            .icon(IconName::EllipsisVertical)
            .disabled(busy)
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |mut m, _, _| {
                let entity1 = entity.clone();
                let entity2 = entity.clone();
                let entity3 = entity.clone();
                let entity4 = entity.clone();
                m = m
                    .item(ramag_ui::menu_item("获取").on_click(move |_, _, app| {
                        entity1.update(app, |this, cx| {
                            this.run_remote_op(RemoteOp::Fetch, cx);
                        });
                    }))
                    .item(
                        ramag_ui::menu_item_with_disabled(pull_label.clone(), !has_head).on_click(
                            move |_, window, app| {
                                entity2.update(app, |this, cx| {
                                    this.confirm_remote_op(RemoteOp::Pull, window, cx);
                                });
                            },
                        ),
                    )
                    .item(
                        ramag_ui::menu_item_with_disabled(push_label.clone(), !has_head).on_click(
                            move |_, window, app| {
                                entity3.update(app, |this, cx| {
                                    this.confirm_remote_op(RemoteOp::Push, window, cx);
                                });
                            },
                        ),
                    )
                    .separator()
                    .item(
                        ramag_ui::menu_item_with_disabled("强推", !has_head).on_click(
                            move |_, w, app| {
                                entity4.update(app, |this, cx| {
                                    this.confirm_remote_op(RemoteOp::PushForce, w, cx);
                                });
                            },
                        ),
                    );
                m
            })
            .into_any_element()
    }
}
