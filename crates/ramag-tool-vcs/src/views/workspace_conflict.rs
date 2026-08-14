//! 进行中操作横幅与冲突处理按钮。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex,
};
use ramag_domain::entities::RepoOperation;

use super::helpers::{ConflictOp, OperationStep};
use super::vcs_view::VcsView;

impl VcsView {
    pub(super) fn render_op_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(op) = self.status.as_ref().and_then(|s| s.operation) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let danger = theme.danger;
        let mut bg = danger;
        bg.a = 0.15;
        let busy = self.busy;

        let conflicts = self
            .status
            .as_ref()
            .map(|s| s.files.iter().filter(|f| f.is_conflicted()).count())
            .unwrap_or(0);
        let base_title = match op {
            RepoOperation::Merge => "合并进行中",
            RepoOperation::Rebase => "Rebase 进行中",
            RepoOperation::CherryPick => "Cherry-pick 进行中",
            RepoOperation::Revert => "Revert 进行中",
        };
        let title = if conflicts > 0 {
            format!("{base_title} · {conflicts} 个冲突待解决")
        } else {
            base_title.to_string()
        };
        let supports_skip = matches!(op, RepoOperation::Rebase);

        h_flex()
            .w_full()
            .items_center()
            .gap(px(10.0))
            .px(px(14.0))
            .py(px(8.0))
            .bg(bg)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(ramag_ui::icons::git_merge())
                    .small()
                    .text_color(danger),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title),
            )
            .child(
                ramag_ui::clickable_button("vcs-op-continue")
                    .primary()
                    .small()
                    .icon(IconName::Check)
                    .label("继续")
                    .when(conflicts > 0, |button| button.tooltip("请先解决冲突"))
                    .disabled(busy || conflicts > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.confirm_op_step(OperationStep::Continue, window, cx);
                    })),
            )
            .when(supports_skip, |this| {
                this.child(
                    ramag_ui::clickable_button("vcs-op-skip")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowRight)
                        .label("跳过")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.confirm_op_step(OperationStep::Skip, window, cx);
                        })),
                )
            })
            .child(
                ramag_ui::clickable_button("vcs-op-abort")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .label("中止")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.confirm_op_step(OperationStep::Abort, window, cx);
                    })),
            )
            .into_any_element()
    }
}

pub(super) fn conflict_buttons(
    idx: usize,
    path: &str,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> Vec<AnyElement> {
    let path_for_view = path.to_string();
    let view_btn = {
        let id = SharedString::from(format!("vcs-conflict-view-{idx}"));
        ramag_ui::clickable_button(id)
            .ghost()
            .xsmall()
            .icon(ramag_ui::icons::columns_2())
            .tooltip("查看冲突")
            .disabled(busy)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.open_conflict_editor(path_for_view.clone(), cx);
            }))
            .into_any_element()
    };
    vec![
        view_btn,
        conflict_btn(
            "use-ours",
            idx,
            path,
            "采纳左侧".into(),
            IconName::ArrowLeft,
            ConflictOp::UseOurs,
            busy,
            cx,
        ),
        conflict_btn(
            "use-theirs",
            idx,
            path,
            "采纳右侧".into(),
            IconName::ArrowRight,
            ConflictOp::UseTheirs,
            busy,
            cx,
        ),
        conflict_btn(
            "mark-resolved",
            idx,
            path,
            "标记已解决".into(),
            IconName::Check,
            ConflictOp::MarkResolved,
            busy,
            cx,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn conflict_btn(
    kind: &'static str,
    idx: usize,
    path: &str,
    tooltip: String,
    icon: IconName,
    op: ConflictOp,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let id = SharedString::from(format!("vcs-conflict-{kind}-{idx}"));
    let path_owned = path.to_string();
    ramag_ui::clickable_button(id)
        .ghost()
        .xsmall()
        .icon(icon)
        .tooltip(tooltip)
        .disabled(busy)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.run_conflict_op(op, path_owned.clone(), cx);
        }))
        .into_any_element()
}

/// Git index stage 2/3 在不同操作中的真实语义；尤其 Rebase 与直觉中的 ours/theirs 相反。
pub(super) fn conflict_side_labels(
    operation: Option<RepoOperation>,
) -> (&'static str, &'static str) {
    match operation {
        Some(RepoOperation::Merge) => ("当前 HEAD", "待合并分支"),
        Some(RepoOperation::Rebase) => ("Rebase 目标分支", "正在重放的 commit"),
        Some(RepoOperation::CherryPick) => ("当前 HEAD", "Cherry-pick commit"),
        Some(RepoOperation::Revert) => ("当前 HEAD", "Revert 计算结果"),
        None => ("ours / stage 2", "theirs / stage 3"),
    }
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::RepoOperation;

    use super::conflict_side_labels;

    #[test]
    fn rebase_conflict_labels_explain_git_semantics() {
        assert_eq!(
            conflict_side_labels(Some(RepoOperation::Rebase)),
            ("Rebase 目标分支", "正在重放的 commit")
        );
    }
}
