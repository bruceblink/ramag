//! Project Files：排序路径 → 可见行 → uniform_list 行级虚拟化（28px 等高）。
//! 默认全部折叠（IDE 习惯，避免一打开全展开）；状态字母色复用 `helpers::code_letter_color`

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use ramag_domain::entities::{FileChangeKind, FileStatus, contains_case_insensitive};

use super::helpers::{code_letter_color, code_to_letter};
use super::vcs_view::VcsView;

/// uniform_list 行单元，所有变体高度必须等于 28px
#[derive(Clone)]
pub(super) enum ProjectRow {
    /// 目录行：箭头 + 名字，可点击折叠/展开
    Dir {
        name: String,
        dir_path: String,
        depth: usize,
        is_expanded: bool,
    },
    /// 文件行：状态字母 + 名字，可点击查看 diff
    File {
        name: String,
        path_index: usize,
        depth: usize,
    },
}

/// 缓存：三 key (files_version, expanded_version, query) 全等命中复用 rows
pub(super) struct ProjectRowsCacheEntry {
    pub rows: Rc<Vec<ProjectRow>>,
    pub files_version: u64,
    pub expanded_version: u64,
    pub query: String,
}

/// Project 文件行只需展示一个状态字母；按 status Vec 身份缓存路径索引。
pub(super) struct ProjectStatusCacheEntry {
    project_files_version: u64,
    files_identity: usize,
    files_len: usize,
    kinds: Rc<HashMap<usize, FileChangeKind>>,
}

impl ProjectStatusCacheEntry {
    fn get(
        &self,
        project_files_version: u64,
        files_identity: usize,
        files_len: usize,
    ) -> Option<Rc<HashMap<usize, FileChangeKind>>> {
        (self.project_files_version == project_files_version
            && self.files_identity == files_identity
            && self.files_len == files_len)
            .then(|| self.kinds.clone())
    }
}

/// 已排序路径按前缀范围递归生成可见行；折叠目录只生成自身，不物化隐藏后代。
fn build_project_rows(
    project_files: &[String],
    path_indices: &[usize],
    expanded: &std::collections::HashSet<String>,
) -> Vec<ProjectRow> {
    let mut rows = Vec::new();
    flatten_path_range(project_files, path_indices, expanded, "", 0, &mut rows);
    rows
}

fn flatten_path_range(
    project_files: &[String],
    path_indices: &[usize],
    expanded: &std::collections::HashSet<String>,
    parent_path: &str,
    depth: usize,
    out: &mut Vec<ProjectRow>,
) {
    // 第一遍只输出目录，保证每层目录始终排在文件之前。
    let mut cursor = 0usize;
    while cursor < path_indices.len() {
        let Some(path) = project_files.get(path_indices[cursor]) else {
            cursor += 1;
            continue;
        };
        let Some(relative) = relative_project_path(path, parent_path) else {
            cursor += 1;
            continue;
        };
        let Some((name, _)) = relative.split_once('/') else {
            cursor += 1;
            continue;
        };
        let mut end = cursor + 1;
        while end < path_indices.len() {
            let same_directory = project_files
                .get(path_indices[end])
                .and_then(|path| relative_project_path(path, parent_path))
                .and_then(|relative| relative.split_once('/').map(|(dir, _)| dir))
                == Some(name);
            if !same_directory {
                break;
            }
            end += 1;
        }
        let dir_path = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{parent_path}/{name}")
        };
        let is_expanded = expanded.contains(&dir_path);
        out.push(ProjectRow::Dir {
            name: name.to_string(),
            dir_path: dir_path.clone(),
            depth,
            is_expanded,
        });
        if is_expanded {
            flatten_path_range(
                project_files,
                &path_indices[cursor..end],
                expanded,
                &dir_path,
                depth + 1,
                out,
            );
        }
        cursor = end;
    }

    // 第二遍输出当前层直属文件；深层路径已由上面的目录范围负责。
    for &path_index in path_indices {
        let Some(path) = project_files.get(path_index) else {
            continue;
        };
        let Some(relative) = relative_project_path(path, parent_path) else {
            continue;
        };
        if !relative.contains('/') {
            out.push(ProjectRow::File {
                name: relative.to_string(),
                path_index,
                depth,
            });
        }
    }
}

fn relative_project_path<'a>(path: &'a str, parent_path: &str) -> Option<&'a str> {
    if parent_path.is_empty() {
        Some(path)
    } else {
        path.strip_prefix(parent_path)?.strip_prefix('/')
    }
}

impl VcsView {
    pub(super) fn prune_project_expanded_dirs(&mut self) {
        if self.project_expanded_dirs.is_empty() {
            return;
        }
        let current = collect_ancestors(&self.project_files);
        let before = self.project_expanded_dirs.len();
        self.project_expanded_dirs
            .retain(|path| current.contains(path));
        if self.project_expanded_dirs.len() != before {
            self.project_expanded_dirs_version = self.project_expanded_dirs_version.wrapping_add(1);
            self.project_rows_cache.get_mut().take();
        }
    }

    /// Project Files 视图主入口（IDE 左侧 panel Project 模式）
    pub(super) fn render_project_files_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;

        if self.loading_project_files {
            return div()
                .px(px(2.0))
                .py(px(8.0))
                .text_sm()
                .text_color(muted_fg)
                .child("加载中…")
                .into_any_element();
        }

        // 文件路径过滤（与 Changes 视图共用同一搜索框）
        let query = self
            .files_search_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();

        // 三 key 全等命中缓存复用 rows，跳过 build_tree + flatten
        let rows_rc: Rc<Vec<ProjectRow>> = {
            let cache = self.project_rows_cache.borrow();
            let hit = cache.as_ref().filter(|e| {
                e.files_version == self.project_files_version
                    && e.expanded_version == self.project_expanded_dirs_version
                    && e.query == query
            });
            if let Some(entry) = hit {
                entry.rows.clone()
            } else {
                drop(cache);
                self.rebuild_project_rows(&query)
            }
        };

        // 空仓库 / 无匹配：缓存内 rows 也是空，给独立占位
        if rows_rc.is_empty() {
            let msg = if self.project_files.is_empty() {
                "(空仓库 / 全部文件被 .gitignore 排除)"
            } else {
                "(无匹配的文件，试着修改搜索关键词)"
            };
            return div()
                .px(px(2.0))
                .py(px(8.0))
                .text_sm()
                .text_color(muted_fg)
                .child(msg)
                .into_any_element();
        }

        // uniform_list 行级虚拟化：仅渲染屏幕可见行，万级文件也流畅
        let status_kinds = self.project_status_kinds();
        let body = uniform_list(
            "vcs-project-files",
            rows_rc.len(),
            cx.processor({
                let rows_rc = rows_rc.clone();
                let status_kinds = status_kinds.clone();
                move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| this.render_project_row(i, &rows_rc[i], status_kinds.as_ref(), cx))
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.project_scroll)
        .flex_1();

        v_flex()
            .size_full()
            .min_h_0()
            .child(body)
            .into_any_element()
    }

    fn project_status_kinds(&self) -> Rc<HashMap<usize, FileChangeKind>> {
        let (files_identity, files_len) = self.status.as_ref().map_or((0, 0), |status| {
            (status.files.as_ptr() as usize, status.files.len())
        });
        {
            let cache = self.project_status_cache.borrow();
            if let Some(kinds) = cache
                .as_ref()
                .and_then(|entry| entry.get(self.project_files_version, files_identity, files_len))
            {
                return kinds;
            }
        }

        let kinds = Rc::new(build_status_kind_map(
            &self.project_files,
            self.status
                .as_ref()
                .map_or(&[][..], |status| status.files.as_slice()),
        ));
        *self.project_status_cache.borrow_mut() = Some(ProjectStatusCacheEntry {
            project_files_version: self.project_files_version,
            files_identity,
            files_len,
            kinds: kinds.clone(),
        });
        kinds
    }

    /// 渲染单条扁平行（uniform_list closure 内调用）
    fn render_project_row(
        &self,
        row_index: usize,
        row: &ProjectRow,
        status_kinds: &HashMap<usize, FileChangeKind>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            ProjectRow::Dir {
                name,
                dir_path,
                depth,
                is_expanded,
            } => self.render_pf_dir_row(
                row_index,
                name.clone(),
                dir_path.clone(),
                *depth,
                *is_expanded,
                cx,
            ),
            ProjectRow::File {
                name,
                path_index,
                depth,
            } => self.project_files.get(*path_index).map_or_else(
                || div().h(px(28.0)).into_any_element(),
                |full_path| {
                    self.render_pf_file_row(
                        *path_index,
                        name.clone(),
                        full_path.clone(),
                        *depth,
                        status_kinds.get(path_index).copied(),
                        cx,
                    )
                },
            ),
        }
    }

    /// 目录行：折叠图标 + 名字，整行可点切换展开
    fn render_pf_dir_row(
        &self,
        row_index: usize,
        name: String,
        dir_path: String,
        depth: usize,
        is_expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let hover_bg = theme.muted;
        let arrow = if is_expanded { "▾" } else { "▸" };
        let dir_path_for_toggle = dir_path.clone();
        let row_id = SharedString::from(format!("vcs-pf-dir-{row_index}"));

        h_flex()
            .id(row_id)
            .h(px(28.0))
            .flex_none()
            .w_full()
            .pl(px(4.0 + 12.0 * depth as f32))
            .gap(px(4.0))
            .items_center()
            .cursor_pointer()
            .hover(move |this| this.bg(hover_bg))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_project_dir(dir_path_for_toggle.clone(), cx);
            }))
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .child(arrow),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(super::inline_text_preview(&name, 160)),
            )
            .into_any_element()
    }

    /// 行：状态字母 + 名字。Project 模式点文件走 select_pf_file 看内容，不走 diff
    fn render_pf_file_row(
        &self,
        path_index: usize,
        name: String,
        full_path: String,
        depth: usize,
        status_kind: Option<FileChangeKind>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let hover_bg = theme.muted;
        let mut accent_bg = theme.accent;
        accent_bg.a = 0.10;

        let letter = code_to_letter(status_kind);
        let letter_color = code_letter_color(letter, muted_fg);

        // 选中态用 selected_pf_path（与 selected_file 区分，互不影响）
        let is_selected = self.selected_pf_path.as_deref() == Some(full_path.as_str());

        let path_for_open = full_path.clone();
        let row_id = SharedString::from(format!("vcs-pf-file-{path_index}"));

        let mut row = h_flex()
            .id(row_id)
            .h(px(28.0))
            .flex_none()
            .w_full()
            .pl(px(4.0 + 12.0 * depth as f32))
            .gap(px(6.0))
            .items_center()
            .cursor_pointer()
            .hover(move |this| this.bg(hover_bg))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select_pf_file(path_for_open.clone(), cx);
            }))
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(letter_color)
                    .child(letter),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(fg)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(super::inline_text_preview(&name, 160)),
            );
        if is_selected {
            row = row.bg(accent_bg);
        }
        row.into_any_element()
    }

    /// 缓存 miss 时：过滤并按展开状态生成可见行，结果包 Rc 写入 cache。
    ///
    /// 仅在 (files_version / expanded_version / query) 任一变化时调；命中路径直接复用。
    fn rebuild_project_rows(&self, query: &str) -> Rc<Vec<ProjectRow>> {
        // filter：搜索词非空时按 substring 过滤
        let filtered_indices: Vec<usize> = self
            .project_files
            .iter()
            .enumerate()
            .filter_map(|(index, path)| {
                (query.is_empty() || contains_case_insensitive(path, query)).then_some(index)
            })
            .collect();
        // 搜索时：自动展开所有命中路径的祖先目录
        let auto_expanded: std::collections::HashSet<String> = if query.is_empty() {
            self.project_expanded_dirs.clone()
        } else {
            collect_ancestors_iter(
                filtered_indices
                    .iter()
                    .filter_map(|index| self.project_files.get(*index).map(String::as_str)),
            )
        };
        let rows_rc = Rc::new(build_project_rows(
            &self.project_files,
            &filtered_indices,
            &auto_expanded,
        ));
        // 写回 cache（同一 render 帧内只调一次）
        *self.project_rows_cache.borrow_mut() = Some(ProjectRowsCacheEntry {
            rows: rows_rc.clone(),
            files_version: self.project_files_version,
            expanded_version: self.project_expanded_dirs_version,
            query: query.to_string(),
        });
        rows_rc
    }

    /// 切换 Project Files 目录的折叠状态
    pub(super) fn toggle_project_dir(&mut self, dir_path: String, cx: &mut Context<Self>) {
        self.prune_project_expanded_dirs();
        if !self.project_expanded_dirs.remove(&dir_path) {
            self.project_expanded_dirs.insert(dir_path);
        }
        self.project_expanded_dirs_version = self.project_expanded_dirs_version.wrapping_add(1);
        cx.notify();
    }

    /// 全部展开：把仓库内所有目录路径加入 expanded set（项目大时谨慎使用）
    pub(super) fn expand_all_project_dirs(&mut self, cx: &mut Context<Self>) {
        self.project_expanded_dirs = collect_ancestors(&self.project_files);
        self.project_expanded_dirs_version = self.project_expanded_dirs_version.wrapping_add(1);
        cx.notify();
    }

    /// 全部折叠：清空 expanded set，回到默认状态（仅顶层节点可见）
    pub(super) fn collapse_all_project_dirs(&mut self, cx: &mut Context<Self>) {
        self.project_expanded_dirs.clear();
        self.project_expanded_dirs_version = self.project_expanded_dirs_version.wrapping_add(1);
        cx.notify();
    }
}

/// 文件 FileStatus → 显示状态：未暂存优先（与日常关注一致）；其次暂存；冲突最高优先
fn pick_display_kind(f: &FileStatus) -> Option<FileChangeKind> {
    if f.is_conflicted() {
        return Some(FileChangeKind::Conflicted);
    }
    f.unstaged.or(f.staged)
}

fn build_status_kind_map(
    project_files: &[String],
    files: &[FileStatus],
) -> HashMap<usize, FileChangeKind> {
    let mut kinds = HashMap::with_capacity(files.len());
    for file in files {
        let Some(kind) = pick_display_kind(file) else {
            continue;
        };
        let Ok(path_index) =
            project_files.binary_search_by(|path| path.as_str().cmp(file.path.as_str()))
        else {
            continue;
        };
        kinds.insert(path_index, kind);
    }
    kinds
}

/// 搜索时收集所有命中路径的祖先目录（让用户能看到匹配文件）
///
/// 例：`["a/b/c.rs"]` → `{"a", "a/b"}`
fn collect_ancestors(paths: &[String]) -> std::collections::HashSet<String> {
    collect_ancestors_iter(paths.iter().map(String::as_str))
}

fn collect_ancestors_iter<'a>(
    paths: impl IntoIterator<Item = &'a str>,
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for path in paths {
        let mut parts = path.split('/').peekable();
        let mut ancestor = String::new();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                break;
            }
            if !ancestor.is_empty() {
                ancestor.push('/');
            }
            ancestor.push_str(part);
            set.insert(ancestor.clone());
        }
    }
    set
}

#[cfg(test)]
#[path = "project_files/tests.rs"]
mod tests;
