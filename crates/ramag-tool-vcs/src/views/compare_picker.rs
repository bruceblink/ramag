//! 分支比较端点选择器：复用当前仓库的本地、远程分支数据，保持操作只读。

use std::collections::BTreeMap;

use gpui::{AnyElement, Context, Entity, IntoElement, SharedString, Styled as _, Window, px};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    menu::{PopupMenu, PopupMenuItem},
};

use super::vcs_view::VcsView;
use ramag_ui::PointerDropdownMenu as _;

#[derive(Clone, Copy)]
pub(super) enum CompareSide {
    From,
    To,
}

impl CompareSide {
    fn id(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::To => "to",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::From => "基线",
            Self::To => "目标",
        }
    }
}

type CompareBranch = (String, String, bool);

#[derive(Clone)]
struct CompareBranchMenuContext {
    selected_revision: String,
    side: CompareSide,
    entity: Entity<VcsView>,
}

impl CompareBranchMenuContext {
    fn new(selected_revision: &str, side: CompareSide, entity: Entity<VcsView>) -> Self {
        Self {
            selected_revision: selected_revision.to_string(),
            side,
            entity,
        }
    }

    /// 添加一个只读比较分支项；选中项保留勾选，远程项使用箭头提示来源。
    fn push_leaf(
        &self,
        menu: PopupMenu,
        full_name: &str,
        display: &str,
        revision: &str,
        is_head: bool,
        is_remote: bool,
    ) -> PopupMenu {
        let prefix = if revision == self.selected_revision {
            "✓  "
        } else if is_remote {
            "↗  "
        } else if is_head {
            "●  "
        } else {
            "    "
        };
        let label = format!("{prefix}{}", super::inline_text_preview(display, 40));
        let name = full_name.to_string();
        let revision = revision.to_string();
        let side = self.side;
        let entity = self.entity.clone();
        menu.item(ramag_ui::menu_item(label).on_click(move |_, _, app| {
            entity.update(app, |this, cx| {
                this.select_compare_branch(side, name.clone(), revision.clone(), cx);
            });
        }))
    }
}

impl VcsView {
    /// 替换比较的一端并重新加载范围文件；不会改变当前工作树或执行 checkout。
    fn select_compare_branch(
        &mut self,
        side: CompareSide,
        label: String,
        commit: String,
        cx: &mut Context<Self>,
    ) {
        let Some(compare) = self.compare.as_ref() else {
            return;
        };
        let (from_label, from, to_label, to) = match side {
            CompareSide::From => (label, commit, compare.to_label.clone(), compare.to.clone()),
            CompareSide::To => (
                compare.from_label.clone(),
                compare.from.clone(),
                label,
                commit,
            ),
        };
        if from == compare.from && to == compare.to {
            if let Some(compare) = self.compare.as_mut() {
                compare.from_label = from_label;
                compare.to_label = to_label;
            }
            cx.notify();
            return;
        }
        self.open_compare_revisions(from_label, from, to_label, to, cx);
    }

    /// 交换基线与目标 revision，保持只读比较方向与文件选择状态的一致性。
    pub(super) fn swap_compare_revisions(&mut self, cx: &mut Context<Self>) {
        let Some(compare) = self.compare.as_ref() else {
            return;
        };
        self.open_compare_revisions(
            compare.to_label.clone(),
            compare.to.clone(),
            compare.from_label.clone(),
            compare.from.clone(),
            cx,
        );
    }

    /// 渲染比较端点选择器；菜单只改变比较参数，不触发 checkout 或其他 Git 写操作。
    pub(super) fn render_compare_revision_picker(
        &self,
        side: CompareSide,
        label: &str,
        commit: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (local_limit, remote_limit) = super::branch_picker::branch_picker_limits(
            self.local_branches.len(),
            self.remote_branches.len(),
        );
        let local = self
            .local_branches
            .iter()
            .take(local_limit)
            .map(|branch| (branch.name.clone(), branch.commit.0.clone(), branch.is_head))
            .collect::<Vec<_>>();
        let remote = self
            .remote_branches
            .iter()
            .take(remote_limit)
            .map(|branch| (branch.name.clone(), branch.commit.0.clone(), false))
            .collect::<Vec<_>>();
        let detached_head = self.status.as_ref().and_then(|status| {
            (status.head_branch.is_none())
                .then(|| status.head_commit.clone())
                .flatten()
        });
        let selected_commit = commit.to_string();
        let entity = cx.entity();
        let branch_context = CompareBranchMenuContext::new(&selected_commit, side, entity);
        let button_label = format!(
            "{}: {} ▾",
            side.label(),
            super::inline_text_preview(label, 8)
        );
        let tooltip = format!("选择{}：{}", side.label(), label);

        ramag_ui::clickable_button(SharedString::from(format!(
            "vcs-compare-side-{}",
            side.id()
        )))
        .outline()
        .xsmall()
        .w_full()
        .label(button_label)
        .text_color(theme.foreground)
        .tooltip(tooltip)
        .pointer_dropdown_menu_with_anchor(
            gpui::Anchor::BottomLeft,
            move |mut menu: PopupMenu, window, cx| {
                menu = menu.max_w(px(420.0));
                menu = menu.item(PopupMenuItem::label(format!("选择{}分支", side.label())));
                if let Some(head_commit) = detached_head.clone() {
                    menu = branch_context.push_leaf(
                        menu,
                        "HEAD",
                        "HEAD（当前）",
                        &head_commit,
                        true,
                        false,
                    );
                    menu = menu.separator();
                }
                if !local.is_empty() {
                    menu = menu.item(PopupMenuItem::label("本地"));
                    menu = render_compare_branches_grouped(
                        menu,
                        &local,
                        false,
                        branch_context.clone(),
                        window,
                        cx,
                    );
                }
                if !remote.is_empty() {
                    menu = menu.separator().item(PopupMenuItem::label("远程"));
                    menu = render_compare_branches_grouped(
                        menu,
                        &remote,
                        true,
                        branch_context.clone(),
                        window,
                        cx,
                    );
                }
                menu
            },
        )
        .into_any_element()
    }
}

/// 按远程名或本地分支前缀组织比较菜单，避免大量分支把选择器撑得过高。
fn render_compare_branches_grouped(
    mut menu: PopupMenu,
    items: &[CompareBranch],
    is_remote: bool,
    branch_context: CompareBranchMenuContext,
    window: &mut Window,
    cx: &mut gpui::Context<PopupMenu>,
) -> PopupMenu {
    let mut singles = Vec::new();
    let mut groups: BTreeMap<String, Vec<(String, String, bool)>> = BTreeMap::new();
    for (name, revision, is_head) in items {
        if let Some(slash) = name.find('/') {
            groups.entry(name[..slash].to_string()).or_default().push((
                name[slash + 1..].to_string(),
                revision.clone(),
                *is_head,
            ));
        } else {
            singles.push((name.clone(), revision.clone(), *is_head));
        }
    }
    for (name, revision, is_head) in &singles {
        menu = branch_context.push_leaf(menu, name, name, revision, *is_head, is_remote);
    }
    for (prefix, group_items) in groups {
        let branch_context_for_sub = branch_context.clone();
        let prefix_for_sub = prefix.clone();
        menu = menu.submenu(
            SharedString::from(prefix),
            window,
            cx,
            move |mut submenu, _window, _cx| {
                submenu = submenu.scrollable(true).max_h(px(360.0));
                for (rest, revision, is_head) in &group_items {
                    let full_name = format!("{prefix_for_sub}/{rest}");
                    submenu = branch_context_for_sub
                        .push_leaf(submenu, &full_name, rest, revision, *is_head, is_remote);
                }
                submenu
            },
        );
    }
    menu
}
