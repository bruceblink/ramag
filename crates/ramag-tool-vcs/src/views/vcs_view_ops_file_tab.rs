//! 文件标签与未跟踪文件预览。

use gpui::Context;
use ramag_domain::entities::DiffKind;
use tracing::error;

use super::helpers::{FileTab, FileTabSource, GroupKind};
use super::vcs_view::VcsView;
use super::vcs_view_ops_repo::read_raw_file_content;

mod data;

/// 当前仓库所有文件标签的正文缓存预算；活动标签始终保留，非活动标签按最近打开顺序保留。
const FILE_TAB_CACHE_BYTE_BUDGET: usize = 96 * 1024 * 1024;

impl VcsView {
    /// 选中文件并加载 diff，优先复用已有标签缓存。
    pub(super) fn select_file(&mut self, path: String, kind: GroupKind, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        self.capture_active_project_draft(cx);
        let existing = self
            .file_tabs
            .iter()
            .position(|t| t.path == path && t.source == FileTabSource::Changes(kind));
        self.diff_request_seq = self.diff_request_seq.wrapping_add(1);
        let request_seq = self.diff_request_seq;
        // 切换文件时重置阅读视图。
        let same_file = self.selected_file.as_ref() == Some(&(path.clone(), kind));
        if !same_file {
            self.reset_blame_context();
            self.expanded_diff_spacers.clear();
            self.diff_h_scroll
                .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
            self.diff_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
            self.diff_scroll_gesture.reset();
        }
        // 切换到变更文件时关闭提交详情。
        if self.viewing_commit.is_some() {
            self.commit_detail_request_seq = self.commit_detail_request_seq.wrapping_add(1);
            self.viewing_commit = None;
            self.reset_commit_files_tree();
            self.selected_commit_file = None;
            self.commit_file_diff = None;
            self.loading_commit_files = false;
        }
        if let Some(idx) = existing {
            self.active_file_tab_idx = Some(idx);
            self.selected_file = Some((path.clone(), kind));
            self.selected_pf_path = None;
            self.current_file_content = None;
            if let Some(cached) = self.file_tabs[idx].cached_diff.clone() {
                self.current_diff = Some(cached);
                self.current_diff_syntax = self.file_tabs[idx].cached_diff_syntax.clone();
                self.loading_diff = false;
                cx.notify();
                return;
            }
            self.current_diff = None;
            self.current_diff_syntax = None;
            self.loading_diff = true;
        } else {
            self.file_tabs.push(FileTab {
                path: path.clone(),
                source: FileTabSource::Changes(kind),
                cached_diff: None,
                cached_diff_syntax: None,
                cached_content: None,
            });
            self.active_file_tab_idx = Some(self.file_tabs.len() - 1);
            self.selected_file = Some((path.clone(), kind));
            self.selected_pf_path = None;
            self.current_file_content = None;
            self.current_diff = None;
            self.current_diff_syntax = None;
            self.loading_diff = true;
            self.scroll_file_tabs_to_end();
        }
        cx.notify();

        let diff_kind = match kind {
            GroupKind::Staged => DiffKind::IndexVsHead,
            GroupKind::Unstaged => DiffKind::WorkingTreeVsIndex,
            // Untracked 不在 index：git diff 无输出 → 读盘构造「全新增」伪 diff 预览
            GroupKind::Untracked => {
                self.load_untracked_preview(path, request_seq, cx);
                return;
            }
            // Conflict 走三栏解决器（左侧行点击直达），diff 区仅给提示
            GroupKind::Conflict => {
                self.loading_diff = false;
                cx.notify();
                return;
            }
        };
        let driver = self.driver.clone();
        let path_for_diff = path.clone();
        let context_lines = self.diff_view_mode.context_lines();
        let source_for_diff = FileTabSource::Changes(kind);
        cx.spawn(async move |this, cx| {
            let result = driver
                .diff_file_full_opts(&repo, &path_for_diff, diff_kind, false, context_lines)
                .await;
            let result = match result {
                Ok(diff) => {
                    let syntax_path = path_for_diff.clone();
                    ramag_app::run_blocking(move || {
                        let syntax = super::syntax::DiffSyntaxSnapshot::new_bounded(
                            &diff,
                            super::syntax::lang_for_path(&syntax_path),
                        );
                        Ok((diff, syntax))
                    })
                    .await
                }
                Err(error) => Err(error),
            };
            let _ =
                this.update(cx, |this, cx| {
                    if !this.is_current_repo(&repo) || this.diff_request_seq != request_seq {
                        return;
                    }
                    this.loading_diff = false;
                    match result {
                        Ok((d, syntax)) => {
                            let d = std::rc::Rc::new(d);
                            let syntax = syntax.map(std::rc::Rc::new);
                            let still_current =
                                this.selected_file.as_ref() == Some(&(path_for_diff.clone(), kind));
                            if still_current {
                                this.current_diff = Some(d.clone());
                                this.current_diff_syntax = syntax.clone();
                            }
                            // 按路径和来源回写，避免关闭标签后索引失效。
                            if let Some(tab) = this.file_tabs.iter_mut().find(|tab| {
                                tab.path == path_for_diff && tab.source == source_for_diff
                            }) {
                                tab.cached_diff = Some(d);
                                tab.cached_diff_syntax = syntax;
                            }
                            this.prune_file_tab_payloads();
                        }
                        Err(e) => {
                            error!(
                                operation = "vcs_diff_load",
                                repo_id = %repo,
                                path = %path_for_diff,
                                error = %e,
                                "diff failed"
                            );
                            if this.selected_file.as_ref() == Some(&(path_for_diff.clone(), kind)) {
                                this.error = Some(format!("拉取 diff 失败：{e}"));
                            }
                        }
                    }
                    cx.notify();
                });
        })
        .detach();
    }

    /// 关闭文件标签。
    pub(super) fn close_file_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.file_tabs.len() {
            return;
        }
        self.capture_active_project_draft(cx);
        if self.file_tabs[idx].is_dirty() {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "文件尚未完成自动保存，请稍后关闭；也可按 {} 立即重试",
                    ramag_ui::platform::primary_shortcut("S")
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }
        self.file_tabs.remove(idx);
        self.diff_request_seq = self.diff_request_seq.wrapping_add(1);
        self.reset_blame_context();
        if self.file_tabs.is_empty() {
            self.active_file_tab_idx = None;
            self.selected_file = None;
            self.current_diff = None;
            self.current_diff_syntax = None;
            self.loading_diff = false;
            self.selected_pf_path = None;
            self.current_file_content = None;
            self.loading_file_content = false;
            self.selected_commit_file = None;
            self.commit_file_diff = None;
            self.diff_fullscreen = false;
        } else {
            let new_idx = match self.active_file_tab_idx {
                Some(i) if i == idx => idx.saturating_sub(1).min(self.file_tabs.len() - 1),
                Some(i) if i > idx => i - 1,
                Some(i) => i,
                None => 0,
            };
            self.active_file_tab_idx = Some(new_idx);
            if let Some(tab) = self.file_tabs.get(new_idx).cloned() {
                match tab.source {
                    FileTabSource::Changes(kind) => self.select_file(tab.path, kind, cx),
                    FileTabSource::ProjectFiles => self.select_pf_file(tab.path, cx),
                    FileTabSource::Commit { commit_id, .. } => {
                        self.select_commit_file(tab.path, commit_id, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// 读取未跟踪文件并构造全新增 diff。
    fn load_untracked_preview(&mut self, path: String, request_seq: u64, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else {
            return;
        };
        let repo_id = repo.id.clone();
        let repo_root = std::path::PathBuf::from(&repo.path);
        cx.spawn(async move |this, cx| {
            let rel_for_worker = path.clone();
            let result = ramag_app::run_blocking(move || {
                let raw = read_raw_file_content(&repo_root, &rel_for_worker);
                match raw.error.clone() {
                    Some(error) => Err(ramag_domain::error::DomainError::Other(error)),
                    None => {
                        let diff = data::build_untracked_diff(raw);
                        let syntax = super::syntax::DiffSyntaxSnapshot::new_bounded(
                            &diff,
                            super::syntax::lang_for_path(&rel_for_worker),
                        );
                        Ok((diff, syntax))
                    }
                }
            })
            .await
            .map_err(|e| e.to_string());
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo_id) || this.diff_request_seq != request_seq {
                    return;
                }
                this.loading_diff = false;
                match result {
                    Ok((diff, syntax)) => {
                        let d = std::rc::Rc::new(diff);
                        let syntax = syntax.map(std::rc::Rc::new);
                        if let Some(tab) = this.file_tabs.iter_mut().find(|t| {
                            t.path == path
                                && t.source == FileTabSource::Changes(GroupKind::Untracked)
                        }) {
                            tab.cached_diff = Some(d.clone());
                            tab.cached_diff_syntax = syntax.clone();
                        }
                        this.prune_file_tab_payloads();
                        let is_selected = this
                            .selected_file
                            .as_ref()
                            .is_some_and(|(p, k)| p == &path && *k == GroupKind::Untracked);
                        if is_selected {
                            this.current_diff = Some(d);
                            this.current_diff_syntax = syntax;
                        }
                    }
                    Err(msg) => {
                        error!(
                            operation = "vcs_untracked_file_load",
                            repo_id = %repo_id,
                            path = %path,
                            error = %msg,
                            "read untracked file failed"
                        );
                        if this.selected_file.as_ref()
                            == Some(&(path.clone(), GroupKind::Untracked))
                        {
                            this.error = Some(format!("读取文件失败：{msg}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 同步活动标签的派生状态。
    pub(super) fn activate_file_tab_state(&mut self, tab: FileTab) {
        match &tab.source {
            FileTabSource::Changes(kind) => {
                self.selected_file = Some((tab.path.clone(), *kind));
                self.current_diff = tab.cached_diff.clone();
                self.current_diff_syntax = tab.cached_diff_syntax.clone();
                self.loading_diff = tab.cached_diff.is_none()
                    && matches!(kind, GroupKind::Staged | GroupKind::Unstaged);
                self.selected_pf_path = None;
                self.current_file_content = None;
                self.loading_file_content = false;
                self.selected_commit_file = None;
            }
            FileTabSource::ProjectFiles => {
                self.selected_pf_path = Some(tab.path.clone());
                self.current_file_content = tab.cached_content.clone();
                self.loading_file_content = tab.cached_content.is_none();
                if let Some(snapshot) = tab.cached_content.as_ref() {
                    self.queue_project_editor_load(snapshot);
                } else {
                    self.pending_pf_editor_load = None;
                    self.pf_editor_loaded_path = None;
                    self.pf_editor_dirty = false;
                    self.pf_editor_line_count = 0;
                }
                self.selected_file = None;
                self.current_diff = None;
                self.current_diff_syntax = None;
                self.loading_diff = false;
                self.selected_commit_file = None;
            }
            FileTabSource::Commit { .. } => {
                self.selected_file = None;
                self.current_diff = tab.cached_diff.clone();
                self.current_diff_syntax = tab.cached_diff_syntax.clone();
                self.loading_diff = tab.cached_diff.is_none();
                self.selected_pf_path = None;
                self.current_file_content = None;
                self.loading_file_content = false;
                self.selected_commit_file = Some(tab.path.clone());
            }
        }
    }

    /// 清理超出预算的非活动标签缓存。
    pub(super) fn prune_file_tab_payloads(&mut self) {
        data::prune_file_tab_payloads_to_budget(
            &mut self.file_tabs,
            self.active_file_tab_idx,
            FILE_TAB_CACHE_BYTE_BUDGET,
        );
    }

    /// 将标签滚动到末尾。
    pub(super) fn scroll_file_tabs_to_end(&self) {
        self.file_tabs_h_scroll
            .set_offset(gpui::point(gpui::px(-99_999.0), gpui::px(0.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::super::helpers::FileContentSnapshot;
    use super::super::vcs_view_ops_repo::RawFileContent;
    use super::data::{
        build_untracked_diff, file_tab_payload_bytes, prune_file_tab_payloads_to_budget,
    };
    use super::*;
    use ramag_domain::entities::DiffLineKind;

    fn raw(lines: Vec<&str>, binary: bool, truncated: bool) -> RawFileContent {
        RawFileContent {
            path: "new.rs".into(),
            lines: lines.into_iter().map(str::to_owned).collect(),
            truncated,
            binary,
            error: None,
        }
    }

    #[test]
    fn untracked_diff_marks_all_lines_added() {
        let d = build_untracked_diff(raw(vec!["a", "b"], false, false));
        assert_eq!(d.hunks.len(), 1);
        let hunk = &d.hunks[0];
        assert_eq!(hunk.new_lines, 2);
        assert!(
            hunk.lines
                .iter()
                .all(|l| matches!(l.kind, DiffLineKind::Add))
        );
        assert_eq!(hunk.lines[1].new_lineno, Some(2));
        assert_eq!(hunk.lines[1].old_lineno, None);
    }

    #[test]
    fn untracked_diff_binary_has_no_hunks() {
        let d = build_untracked_diff(raw(vec![], true, false));
        assert!(d.binary);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn untracked_diff_truncated_sets_heading() {
        let d = build_untracked_diff(raw(vec!["x"], false, true));
        assert!(d.hunks[0].heading.as_deref().unwrap_or("").contains("截断"));
    }

    fn project_tab(path: &str, bytes: usize) -> FileTab {
        let text = "x".repeat(bytes);
        FileTab {
            path: path.to_string(),
            source: FileTabSource::ProjectFiles,
            cached_diff: None,
            cached_diff_syntax: None,
            cached_content: Some(FileContentSnapshot {
                path: path.to_string(),
                text: std::rc::Rc::new(text),
                line_count: 1,
                revision: 0,
                dirty: false,
                truncated: false,
                binary: false,
                error: None,
            }),
        }
    }

    #[test]
    fn file_tab_cache_keeps_active_and_newer_payloads_within_budget() {
        let mut tabs = vec![
            project_tab("old.rs", 128),
            project_tab("active.rs", 128),
            project_tab("new.rs", 128),
        ];
        let budget = file_tab_payload_bytes(&tabs[1]) + file_tab_payload_bytes(&tabs[2]);

        prune_file_tab_payloads_to_budget(&mut tabs, Some(1), budget);

        assert!(tabs[0].cached_content.is_none());
        assert!(tabs[1].cached_content.is_some());
        assert!(tabs[2].cached_content.is_some());
    }

    #[test]
    fn file_tab_cache_never_evicts_unsaved_project_draft() {
        let mut tabs = vec![project_tab("draft.rs", 128), project_tab("active.rs", 128)];
        assert!(tabs[0].cached_content.is_some());
        if let Some(content) = tabs[0].cached_content.as_mut() {
            content.dirty = true;
        }

        prune_file_tab_payloads_to_budget(&mut tabs, Some(1), 0);

        assert!(tabs[0].cached_content.is_some());
        assert!(tabs[1].cached_content.is_some());
    }

    #[test]
    fn cache_pruning_keeps_tab_metadata_without_count_limit() {
        let mut tabs = (0..64)
            .map(|index| project_tab(&format!("file-{index}.rs"), 128))
            .collect::<Vec<_>>();

        prune_file_tab_payloads_to_budget(&mut tabs, Some(63), 0);

        assert_eq!(tabs.len(), 64);
        assert_eq!(tabs[0].path, "file-0.rs");
        assert_eq!(tabs[63].path, "file-63.rs");
        assert!(tabs[63].cached_content.is_some());
    }
}
