//! Commit 详情面板（左侧 sidebar）：commit metadata + 文件树（按目录组织 + 中间空目录压缩）
//!
//! 与 Changes 文件分组共用 [`super::file_tree`] 构建相同的目录结构，保证两边视觉一致

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};
use ramag_domain::entities::{Commit, FileStatus};

use super::file_tree::{Row, build_tree, flatten};
use super::helpers::{code_letter_color, code_to_letter};
use super::vcs_view::VcsView;

#[derive(Clone, Copy, PartialEq, Eq)]
struct CommitFilesRowsCacheKey {
    files_identity: usize,
    files_len: usize,
    collapsed_version: u64,
}

/// Commit 文件树扁平行缓存；选中文件等普通重渲染不再重建整棵目录树。
pub(super) struct CommitFilesRowsCacheEntry {
    key: CommitFilesRowsCacheKey,
    rows: Rc<Vec<Row>>,
}

impl CommitFilesRowsCacheEntry {
    fn get(&self, key: &CommitFilesRowsCacheKey) -> Option<Rc<Vec<Row>>> {
        (self.key == *key).then(|| self.rows.clone())
    }
}

impl VcsView {
    /// Commit 详情面板：close 按钮 + 简略 metadata + 文件树
    pub(super) fn render_commit_detail_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(commit) = self.viewing_commit.clone() else {
            return div().into_any_element();
        };
        let (border, muted_fg, fg, accent) = {
            let t = cx.theme();
            (t.border, t.muted_foreground, t.foreground, t.accent)
        };
        render_left_sidebar(self, &commit, fg, muted_fg, accent, border, cx)
    }

    /// 切换 commit 文件树目录的折叠状态
    pub(super) fn toggle_commit_files_dir(&mut self, dir_path: String, cx: &mut Context<Self>) {
        if !self.commit_files_collapsed.remove(&dir_path) {
            self.commit_files_collapsed.insert(dir_path);
        }
        self.commit_files_collapsed_version = self.commit_files_collapsed_version.wrapping_add(1);
        self.commit_files_rows_cache.get_mut().take();
        cx.notify();
    }

    /// 清空 commit 文件树及派生缓存；调用方自行处理请求代次和其它详情状态。
    pub(super) fn reset_commit_files_tree(&mut self) {
        self.commit_files = Rc::new(Vec::new());
        self.commit_files_collapsed.clear();
        self.commit_files_collapsed_version = self.commit_files_collapsed_version.wrapping_add(1);
        self.commit_files_rows_cache.get_mut().take();
    }

    fn commit_files_rows(&self) -> Rc<Vec<Row>> {
        let key = CommitFilesRowsCacheKey {
            files_identity: Rc::as_ptr(&self.commit_files) as usize,
            files_len: self.commit_files.len(),
            collapsed_version: self.commit_files_collapsed_version,
        };
        {
            let cache = self.commit_files_rows_cache.borrow();
            if let Some(rows) = cache.as_ref().and_then(|entry| entry.get(&key)) {
                return rows;
            }
        }

        let tree = build_tree(&self.commit_files);
        let mut rows = Vec::with_capacity(self.commit_files.len().saturating_mul(2));
        flatten(&tree, 0, "", &self.commit_files_collapsed, &mut rows);
        let rows = Rc::new(rows);
        self.commit_files_rows_cache
            .replace(Some(CommitFilesRowsCacheEntry {
                key,
                rows: rows.clone(),
            }));
        rows
    }
}

fn render_left_sidebar(
    view: &VcsView,
    commit: &Commit,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    border: gpui::Hsla,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let close_btn = ramag_ui::clickable_button("vcs-commit-detail-close")
        .ghost()
        .xsmall()
        .icon(IconName::Close)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.close_commit_detail(cx);
        }));
    let mono = cx.theme().mono_font_family.clone();
    let head = h_flex()
        .items_center()
        .gap(px(6.0))
        .h(px(36.0))
        .flex_none()
        .px(px(8.0))
        .border_b_1()
        .border_color(border)
        .child(close_btn)
        .child(
            div()
                .font_family(mono.clone())
                .text_xs()
                .text_color(accent)
                .child(commit.id.short().to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(muted_fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(format!("{} 个文件", view.commit_files.len())),
        )
        .child({
            let full_sha = commit.id.0.clone();
            ramag_ui::clickable_button("vcs-commit-copy-sha")
                .ghost()
                .xsmall()
                .icon(ramag_ui::icons::copy())
                .tooltip("复制哈希")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(full_sha.clone()));
                    this.notify_success("已复制完整 SHA", cx);
                }))
        });

    // commit 元信息：完整 subject / body / 作者 / 时间——列表行里被截断的内容在此完整可读
    let meta = render_commit_meta(commit, fg, muted_fg, border, mono);
    let body = render_files_tree(view, fg, muted_fg, cx);

    v_flex()
        .size_full()
        .border_l_1()
        .border_color(border)
        .child(head)
        .child(meta)
        .child(body)
        .into_any_element()
}

/// commit 元信息块：subject 完整换行展示，body 限高滚动，作者 + 邮箱 + 绝对时间
fn render_commit_meta(
    commit: &Commit,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
    mono: SharedString,
) -> AnyElement {
    let author = &commit.author;
    let when = author.timestamp.format("%Y-%m-%d %H:%M").to_string();
    let mut meta = v_flex()
        .flex_none()
        .gap(px(4.0))
        .px(px(10.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(border)
        .child(
            div()
                .text_sm()
                .text_color(fg)
                .whitespace_normal()
                .child(commit.subject.clone()),
        );
    let body_text = commit.body.trim().to_string();
    if !body_text.is_empty() {
        meta = meta.child(
            div()
                .id("vcs-commit-detail-body")
                .max_h(px(140.0))
                .overflow_y_scroll()
                .text_xs()
                .text_color(muted_fg)
                .font_family(mono)
                .whitespace_normal()
                .child(body_text),
        );
    }
    meta.child(
        div()
            .text_xs()
            .text_color(muted_fg)
            .child(format!("{} <{}> · {when}", author.name, author.email)),
    )
    .into_any_element()
}

/// 树状文件列表：build_tree → flatten → uniform_list 行级虚拟化渲染
fn render_files_tree(
    view: &VcsView,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    if view.loading_commit_files {
        return v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(muted_fg)
            .child("加载文件列表…")
            .into_any_element();
    }
    if view.commit_files.is_empty() {
        return v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(muted_fg)
            .child("(commit 无文件变更)")
            .into_any_element();
    }
    let commit_id = view
        .viewing_commit
        .as_ref()
        .map(|c| c.id.0.clone())
        .unwrap_or_default();

    let rows_rc = view.commit_files_rows();
    let files_rc = view.commit_files.clone();
    let total = rows_rc.len();
    let scroll = view.commit_files_scroll.clone();
    let commit_id_rc: Rc<String> = Rc::new(commit_id);

    uniform_list(
        "vcs-commit-files",
        total,
        cx.processor({
            let rows_rc = rows_rc.clone();
            let files_rc = files_rc.clone();
            let commit_id_rc = commit_id_rc.clone();
            move |this, range: Range<usize>, _w, cx| {
                range
                    .map(|i| {
                        render_tree_row(
                            i,
                            &rows_rc[i],
                            &files_rc,
                            &this.selected_commit_file,
                            commit_id_rc.as_str(),
                            fg,
                            muted_fg,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>()
            }
        }),
    )
    .track_scroll(&scroll)
    .h_full()
    .flex_1()
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_tree_row(
    idx_in_rows: usize,
    row: &Row,
    files: &Rc<Vec<FileStatus>>,
    selected: &Option<String>,
    commit_id: &str,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let theme = cx.theme();
    let hover_bg = theme.muted;
    let mut sel_bg = theme.accent;
    sel_bg.a = 0.16;

    match row {
        Row::Dir {
            display_name,
            dir_path,
            depth,
            is_collapsed,
            file_count,
        } => {
            let id = SharedString::from(format!("vcs-cd-dir-{idx_in_rows}"));
            let icon = if *is_collapsed { "▸" } else { "▾" };
            let dir_clone = dir_path.clone();
            h_flex()
                .id(id)
                .gap(px(4.0))
                .items_center()
                .py(px(3.0))
                .pr(px(6.0))
                .pl(px((10 + depth * 12) as f32))
                .rounded(px(3.0))
                .cursor_pointer()
                .hover(move |this| this.bg(hover_bg))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_commit_files_dir(dir_clone.clone(), cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .w(px(12.0))
                        .text_xs()
                        .text_color(muted_fg)
                        .child(icon),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(fg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(super::inline_text_preview(display_name, 160)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted_fg)
                        .child(format!("{file_count}")),
                )
                .into_any_element()
        }
        Row::File { idx, depth } => {
            let f = &files[*idx];
            let code = code_to_letter(f.staged);
            let code_color = code_letter_color(code, muted_fg);
            let label = match (&f.old_path, &f.path) {
                (Some(old), new) if old != new => {
                    let old_base = old.rsplit('/').next().unwrap_or(old.as_str());
                    let new_base = new.rsplit('/').next().unwrap_or(new.as_str());
                    format!("{old_base} → {new_base}")
                }
                _ => f
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(f.path.as_str())
                    .to_string(),
            };
            let is_selected = selected.as_deref() == Some(f.path.as_str());
            let path_for_click = f.path.clone();
            let commit_for_click = commit_id.to_string();
            let id = SharedString::from(format!("vcs-cd-file-{idx_in_rows}"));
            let mut row = h_flex()
                .id(id)
                .gap(px(8.0))
                .items_center()
                .py(px(3.0))
                .pr(px(6.0))
                .pl(px((10 + depth * 12 + 12) as f32))
                .rounded(px(3.0))
                .cursor_pointer()
                .hover(move |this| this.bg(hover_bg))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.select_commit_file(path_for_click.clone(), commit_for_click.clone(), cx);
                }))
                .child(
                    div()
                        .flex_none()
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
                        .text_xs()
                        .text_color(fg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(super::inline_text_preview(&label, 200)),
                );
            if is_selected {
                row = row.bg(sel_bg);
            }
            row.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_rows_cache_requires_matching_files_and_collapse_version() {
        let rows = Rc::new(vec![Row::File { idx: 0, depth: 0 }]);
        let key = CommitFilesRowsCacheKey {
            files_identity: 7,
            files_len: 1,
            collapsed_version: 2,
        };
        let cache = CommitFilesRowsCacheEntry {
            key,
            rows: rows.clone(),
        };

        let cached = cache.get(&key);
        assert!(
            cached
                .as_ref()
                .is_some_and(|cached| Rc::ptr_eq(cached, &rows))
        );
        let changed = CommitFilesRowsCacheKey {
            collapsed_version: 3,
            ..key
        };
        assert!(cache.get(&changed).is_none());
    }
}
