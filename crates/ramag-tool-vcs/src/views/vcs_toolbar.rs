//! Files toolbar 远端操作：dropdown（Fetch / Pull / Push / 强推）

use gpui::{AnyElement, ClickEvent, Context, IntoElement, div};
use gpui_component::{
    Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenuItem},
};

use super::helpers::RemoteOp;
use super::vcs_view::VcsView;

impl VcsView {
    /// 同步快捷主按钮：待拉取时显示「Pull ↓N」、待推送时显示「Push ↑N」，一键可达；
    /// 其余情形返回空（完整操作仍在聚合菜单里）
    pub(super) fn render_sync_quick_action(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.repo.is_none() {
            return div().into_any_element();
        }
        let ahead = self.status.as_ref().and_then(|s| s.ahead).unwrap_or(0);
        let behind = self.status.as_ref().and_then(|s| s.behind).unwrap_or(0);
        let diverged = ahead > 0 && behind > 0;
        let (id, label, tip, op): (&'static str, String, String, RemoteOp) = if diverged {
            (
                "vcs-quick-diverged",
                format!("分叉 ↑{ahead} ↓{behind}"),
                "本地与远程都有新 commit；点击查看 Pull 合并风险".to_string(),
                RemoteOp::Pull,
            )
        } else if behind > 0 {
            (
                "vcs-quick-pull",
                format!("Pull ↓{behind}"),
                format!(
                    "拉取远程的 {behind} 个新 commit（{}）",
                    ramag_ui::platform::primary_shortcut("T")
                ),
                RemoteOp::Pull,
            )
        } else if ahead > 0 {
            (
                "vcs-quick-push",
                format!("Push ↑{ahead}"),
                format!(
                    "推送本地的 {ahead} 个 commit（{}）",
                    ramag_ui::platform::primary_shift_shortcut("K")
                ),
                RemoteOp::Push,
            )
        } else {
            return div().into_any_element();
        };
        Button::new(id)
            .primary()
            .xsmall()
            .label(label)
            .tooltip(tip)
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

        let pull_shortcut = ramag_ui::platform::primary_shortcut("T");
        let push_shortcut = ramag_ui::platform::primary_shift_shortcut("K");
        let pull_label = if behind > 0 {
            format!("Pull ↓{behind}（{pull_shortcut}）")
        } else {
            format!("Pull（{pull_shortcut}）")
        };
        let push_label = if ahead > 0 {
            format!("Push ↑{ahead}（{push_shortcut}）")
        } else {
            format!("Push（{push_shortcut}）")
        };

        Button::new("vcs-ops-menu")
            .ghost()
            .xsmall()
            .icon(IconName::EllipsisVertical)
            .tooltip("Git 操作（Fetch / Pull / Push / 强推）")
            .disabled(busy)
            .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |mut m, _, _| {
                let entity1 = entity.clone();
                let entity2 = entity.clone();
                let entity3 = entity.clone();
                let entity4 = entity.clone();
                m = m
                    .item(PopupMenuItem::new("Fetch").on_click(move |_, _, app| {
                        entity1.update(app, |this, cx| {
                            this.run_remote_op(RemoteOp::Fetch, cx);
                        });
                    }))
                    .item(
                        PopupMenuItem::new(pull_label.clone())
                            .disabled(!has_head)
                            .on_click(move |_, window, app| {
                                entity2.update(app, |this, cx| {
                                    this.confirm_remote_op(RemoteOp::Pull, window, cx);
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(push_label.clone())
                            .disabled(!has_head)
                            .on_click(move |_, window, app| {
                                entity3.update(app, |this, cx| {
                                    this.confirm_remote_op(RemoteOp::Push, window, cx);
                                });
                            }),
                    )
                    .separator()
                    .item(PopupMenuItem::new("⚠ 强推").disabled(!has_head).on_click(
                        move |_, w, app| {
                            entity4.update(app, |this, cx| {
                                this.confirm_remote_op(RemoteOp::PushForce, w, cx);
                            });
                        },
                    ));
                m
            })
            .into_any_element()
    }
}
