//! 工作区变更树，使用 28px 等高虚拟列表。

mod rows;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, v_flex,
};
use ramag_domain::entities::{
    FileChangeKind, FileStatus, WorkingTreeStatus, contains_case_insensitive,
};

use super::helpers::{FileOp, GroupKind, code_letter_color, code_to_letter, file_op_button};
use super::vcs_view::VcsView;
use super::workspace_conflict::conflict_buttons;

/// 行高固定 28px：uniform_list 行级虚拟化要求所有行等高（表头 / 目录 / 文件同高）
const ROW_H: f32 = 28.0;

enum ChangeRow {
    Header {
        title: &'static str,
        kind: GroupKind,
        file_indices: Rc<Vec<usize>>,
    },
    Dir {
        display_name: String,
        dir_path: String,
        depth: usize,
        is_collapsed: bool,
        file_count: usize,
    },
    File {
        file_index: usize,
        depth: usize,
        kind: GroupKind,
    },
}

#[derive(Clone, PartialEq, Eq)]
struct WorkspaceRowsCacheKey {
    status_request_seq: u64,
    files_identity: usize,
    files_len: usize,
    collapsed_version: u64,
    query: String,
}

/// Changes 面板扁平行缓存；不依赖主题和选中态，可跨普通重渲染复用。
pub(super) struct WorkspaceRowsCacheEntry {
    key: WorkspaceRowsCacheKey,
    rows: Rc<Vec<ChangeRow>>,
}

impl WorkspaceRowsCacheEntry {
    fn get(&self, key: &WorkspaceRowsCacheKey) -> Option<Rc<Vec<ChangeRow>>> {
        (self.key == *key).then(|| self.rows.clone())
    }
}

impl VcsView {
    /// 状态刷新后移除已不存在目录的折叠记录，避免长期运行中重命名路径持续累积。
    pub(super) fn prune_changes_collapsed_dirs(&mut self) {
        if self.changes_collapsed_dirs.is_empty() {
            return;
        }
        let current = self
            .status
            .as_ref()
            .map(|status| collect_parent_dirs(status.files.iter().map(|file| file.path.as_str())))
            .unwrap_or_default();
        let before = self.changes_collapsed_dirs.len();
        self.changes_collapsed_dirs
            .retain(|path| current.contains(path));
        if self.changes_collapsed_dirs.len() != before {
            self.changes_collapsed_dirs_version =
                self.changes_collapsed_dirs_version.wrapping_add(1);
            self.changes_rows_cache.get_mut().take();
        }
    }

    pub(super) fn render_file_groups(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;

        let Some(status) = &self.status else {
            self.changes_rows_cache.borrow_mut().take();
            return div().into_any_element();
        };

        let query = self
            .files_search_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let key = WorkspaceRowsCacheKey {
            status_request_seq: self.status_request_seq,
            files_identity: status.files.as_ptr() as usize,
            files_len: status.files.len(),
            collapsed_version: self.changes_collapsed_dirs_version,
            query: query.clone(),
        };
        let rows_rc = {
            let cache = self.changes_rows_cache.borrow();
            if let Some(rows) = cache.as_ref().and_then(|entry| entry.get(&key)) {
                rows
            } else {
                drop(cache);
                self.rebuild_workspace_rows(status, &query, key)
            }
        };

        if rows_rc.is_empty() {
            let msg = if query.is_empty() {
                "✓ 工作区干净，无任何变更"
            } else {
                "（无匹配的变更文件，试着修改搜索关键词）"
            };
            return div()
                .px(px(2.0))
                .py(px(8.0))
                .text_sm()
                .text_color(muted_fg)
                .child(msg)
                .into_any_element();
        }
        let total = rows_rc.len();
        let body = uniform_list(
            "vcs-changes-rows",
            total,
            cx.processor({
                let rows_rc = rows_rc.clone();
                move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| this.render_change_row(i, &rows_rc[i], cx))
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.changes_scroll)
        .flex_1();

        // size_full + min_h_0：在外层 overflow_y_scrollbar 容器内拿到确定高度（同 project_files）
        v_flex()
            .size_full()
            .min_h_0()
            .child(body)
            .into_any_element()
    }

    /// 缓存 miss 时才做路径过滤、四组克隆与目录树扁平化。
    fn rebuild_workspace_rows(
        &self,
        status: &WorkingTreeStatus,
        query: &str,
        key: WorkspaceRowsCacheKey,
    ) -> Rc<Vec<ChangeRow>> {
        let path_match = |path: &str| contains_case_insensitive(path, query);
        let mut staged: Vec<usize> = Vec::new();
        let mut unstaged: Vec<usize> = Vec::new();
        let mut untracked: Vec<usize> = Vec::new();
        let mut conflicted: Vec<usize> = Vec::new();
        for (file_index, file) in status.files.iter().enumerate() {
            if !path_match(&file.path) {
                continue;
            }
            if file.is_conflicted() {
                conflicted.push(file_index);
                continue;
            }
            if file.staged.is_some() {
                staged.push(file_index);
            }
            match file.unstaged {
                Some(FileChangeKind::Untracked) => untracked.push(file_index),
                Some(_) => unstaged.push(file_index),
                None => {}
            }
        }

        let mut rows: Vec<ChangeRow> = Vec::new();
        self.append_change_group(
            "冲突",
            GroupKind::Conflict,
            &status.files,
            conflicted,
            &mut rows,
        );
        self.append_change_group(
            "已暂存",
            GroupKind::Staged,
            &status.files,
            staged,
            &mut rows,
        );
        self.append_change_group(
            "未暂存",
            GroupKind::Unstaged,
            &status.files,
            unstaged,
            &mut rows,
        );
        self.append_change_group(
            "未跟踪",
            GroupKind::Untracked,
            &status.files,
            untracked,
            &mut rows,
        );

        let rows = Rc::new(rows);
        *self.changes_rows_cache.borrow_mut() = Some(WorkspaceRowsCacheEntry {
            key,
            rows: rows.clone(),
        });
        rows
    }

    fn append_change_group(
        &self,
        title: &'static str,
        kind: GroupKind,
        files: &[FileStatus],
        file_indices: Vec<usize>,
        out: &mut Vec<ChangeRow>,
    ) {
        if file_indices.is_empty() {
            return;
        }
        let file_indices = Rc::new(file_indices);
        out.push(ChangeRow::Header {
            title,
            kind,
            file_indices: file_indices.clone(),
        });
        let tree = super::file_tree::build_tree_for_indices(files, &file_indices);
        let mut trows: Vec<super::file_tree::Row> = Vec::with_capacity(file_indices.len() * 2);
        super::file_tree::flatten(&tree, 0, "", &self.changes_collapsed_dirs, &mut trows);
        for r in trows {
            match r {
                super::file_tree::Row::Dir {
                    display_name,
                    dir_path,
                    depth,
                    is_collapsed,
                    file_count,
                } => out.push(ChangeRow::Dir {
                    display_name,
                    dir_path,
                    depth,
                    is_collapsed,
                    file_count,
                }),
                super::file_tree::Row::File { idx, depth } => out.push(ChangeRow::File {
                    file_index: idx,
                    depth,
                    kind,
                }),
            }
        }
    }

    pub(super) fn toggle_changes_dir(&mut self, dir_path: String, cx: &mut Context<Self>) {
        self.prune_changes_collapsed_dirs();
        if !self.changes_collapsed_dirs.remove(&dir_path) {
            self.changes_collapsed_dirs.insert(dir_path);
        }
        self.changes_collapsed_dirs_version = self.changes_collapsed_dirs_version.wrapping_add(1);
        self.changes_rows_cache.get_mut().take();
        cx.notify();
    }

    pub(super) fn render_file_row(
        &self,
        idx: usize,
        f: &FileStatus,
        kind: GroupKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let hover_bg = theme.muted;
        let mut selected_bg = theme.accent;
        selected_bg.a = 0.16;

        let code_kind = match kind {
            GroupKind::Staged => f.staged,
            GroupKind::Unstaged | GroupKind::Untracked => f.unstaged,
            GroupKind::Conflict => f.staged.or(f.unstaged),
        };
        let code = code_to_letter(code_kind);
        let code_color = code_letter_color(code, muted_fg);

        let path_label = match (&f.old_path, &f.path) {
            (Some(old), new) if old != new => format!("{old} → {new}"),
            _ => f.path.clone(),
        };

        let path_for_buttons = f.path.clone();
        let path_for_click = f.path.clone();
        let busy = self.busy;
        let is_selected = self
            .selected_file
            .as_ref()
            .map(|(p, k)| p == &f.path && *k == kind)
            .unwrap_or(false);
        let buttons: Vec<AnyElement> = match kind {
            GroupKind::Staged => vec![file_op_button(
                ("unstage", idx),
                "取消暂存",
                FileOp::Unstage,
                path_for_buttons.clone(),
                busy,
                cx,
            )],
            GroupKind::Unstaged => vec![
                file_op_button(
                    ("stage", idx),
                    "暂存",
                    FileOp::Stage,
                    path_for_buttons.clone(),
                    busy,
                    cx,
                ),
                file_op_button(
                    ("discard", idx),
                    "丢弃",
                    FileOp::Discard,
                    path_for_buttons.clone(),
                    busy,
                    cx,
                ),
            ],
            GroupKind::Untracked => vec![file_op_button(
                ("stage-u", idx),
                "暂存",
                FileOp::Stage,
                path_for_buttons.clone(),
                busy,
                cx,
            )],
            GroupKind::Conflict => conflict_buttons(idx, &f.path, busy, cx),
        };

        // 「查看历史」按钮：所有非 Untracked 文件都可看（untracked 文件还没进 git，无历史）
        let history_btn: Option<AnyElement> = if matches!(kind, GroupKind::Untracked) {
            None
        } else {
            let path_for_history = f.path.clone();
            let id = SharedString::from(format!("vcs-file-history-{idx}-{kind:?}"));
            Some(
                ramag_ui::clickable_button(id)
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::scroll_text())
                    .tooltip("历史")
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.view_file_history(path_for_history.clone(), cx);
                    }))
                    .into_any_element(),
            )
        };
        let mut buttons = buttons;
        if let Some(b) = history_btn {
            buttons.insert(0, b);
        }

        let row_id = SharedString::from(format!("vcs-file-{idx}-{kind:?}"));
        let mut row = h_flex()
            .id(row_id)
            .h(px(ROW_H))
            .flex_none()
            .w_full()
            .gap(px(8.0))
            .items_center()
            .px(px(4.0))
            .rounded(px(3.0))
            .cursor_pointer()
            .hover(move |this| this.bg(hover_bg))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                // 冲突文件：点击行直达三栏解决器（diff 区无法表达三方内容）
                if matches!(kind, GroupKind::Conflict) {
                    this.open_conflict_editor(path_for_click.clone(), cx);
                } else {
                    this.select_file(path_for_click.clone(), kind, cx);
                }
            }))
            .child(Icon::new(IconName::File).xsmall().text_color(muted_fg))
            .child(
                div()
                    .w(px(14.0))
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(code_color)
                    .child(code),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(fg)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(super::inline_text_preview(&path_label, 240)),
            )
            .child(
                h_flex()
                    .gap(px(4.0))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .children(buttons),
            );
        if is_selected {
            row = row.bg(selected_bg);
        }
        row
    }
}

fn collect_parent_dirs<'a>(paths: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    let mut dirs = HashSet::new();
    for path in paths {
        let mut parent = String::new();
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                break;
            }
            if !parent.is_empty() {
                parent.push('/');
            }
            parent.push_str(part);
            dirs.insert(parent.clone());
        }
    }
    dirs
}

#[allow(clippy::too_many_arguments)]
fn bulk_op_button(
    kind: &'static str,
    title: &'static str,
    label: &'static str,
    op: FileOp,
    icon: IconName,
    file_indices: Rc<Vec<usize>>,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let id = SharedString::from(format!("vcs-bulk-{kind}-{title}"));
    ramag_ui::clickable_button(id)
        .ghost()
        .xsmall()
        .icon(icon)
        .label(label)
        .disabled(busy)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let paths = this.status.as_ref().map_or_else(Vec::new, |status| {
                file_indices
                    .iter()
                    .filter_map(|index| status.files.get(*index).map(|file| file.path.clone()))
                    .collect()
            });
            if !paths.is_empty() {
                this.run_file_op(op, paths, cx);
            }
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests;
