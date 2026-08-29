//! 分支比较文件的标签管理与只读 Diff 加载。

use gpui::Context;
use ramag_domain::entities::{CommitId, DiffKind};
use tracing::error;

use super::helpers::{FileTab, FileTabSource};
use super::vcs_view::VcsView;

impl VcsView {
    /// 选择比较文件并按需加载两个 revision 之间的只读 Diff。
    pub(super) fn select_compare_file(
        &mut self,
        path: String,
        from: String,
        to: String,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.as_ref().map(|repo| repo.id.clone()) else {
            return;
        };
        self.capture_active_project_draft(cx);
        let source = FileTabSource::Compare {
            from: from.clone(),
            to: to.clone(),
        };
        let existing = self
            .file_tabs
            .iter()
            .position(|tab| tab.path == path && tab.source == source);
        let is_same_target = existing.is_some_and(|index| {
            self.active_file_tab_idx == Some(index)
                && self
                    .file_tabs
                    .get(index)
                    .is_some_and(|tab| tab.path == path && tab.source == source)
        });
        if !is_same_target {
            self.reset_blame_context();
            self.expanded_diff_spacers.clear();
            self.diff_h_scroll
                .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
            self.diff_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
            self.diff_scroll_gesture.reset();
        }
        if self.viewing_commit.is_some() {
            self.commit_detail_request_seq = self.commit_detail_request_seq.wrapping_add(1);
            self.viewing_commit = None;
            self.reset_commit_files_tree();
            self.selected_commit_file = None;
            self.commit_file_diff = None;
            self.loading_commit_files = false;
        }
        self.diff_request_seq = self.diff_request_seq.wrapping_add(1);
        let request_seq = self.diff_request_seq;
        let index = existing.unwrap_or_else(|| {
            self.file_tabs.push(FileTab {
                path: path.clone(),
                source: source.clone(),
                cached_diff: None,
                cached_diff_syntax: None,
                cached_content: None,
            });
            self.file_tabs.len() - 1
        });
        self.active_file_tab_idx = Some(index);
        self.selected_file = None;
        self.selected_commit_file = None;
        self.selected_pf_path = None;
        self.current_file_content = None;
        self.commit_file_diff = None;
        if let Some(cached) = self.file_tabs[index].cached_diff.clone() {
            self.current_diff = Some(cached);
            self.current_diff_syntax = self.file_tabs[index].cached_diff_syntax.clone();
            self.loading_diff = false;
            cx.notify();
            return;
        }
        self.current_diff = None;
        self.current_diff_syntax = None;
        self.loading_diff = true;
        cx.notify();

        let driver = self.driver.clone();
        let path_for_diff = path.clone();
        let source_for_diff = source.clone();
        let context_lines = self.diff_view_mode.context_lines();
        cx.spawn(async move |this, cx| {
            let result = driver
                .diff_file_full_opts(
                    &repo,
                    &path_for_diff,
                    DiffKind::Range {
                        from: CommitId(from.clone()),
                        to: CommitId(to.clone()),
                    },
                    false,
                    context_lines,
                )
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
                        Ok((diff, syntax)) => {
                            let diff = std::rc::Rc::new(diff);
                            let syntax = syntax.map(std::rc::Rc::new);
                            if let Some(tab) = this.file_tabs.iter_mut().find(|tab| {
                                tab.path == path_for_diff && tab.source == source_for_diff
                            }) {
                                tab.cached_diff = Some(diff.clone());
                                tab.cached_diff_syntax = syntax.clone();
                            }
                            this.prune_file_tab_payloads();
                            let selected = this
                                .active_file_tab_idx
                                .and_then(|index| this.file_tabs.get(index))
                                .is_some_and(|tab| {
                                    tab.path == path_for_diff && tab.source == source_for_diff
                                });
                            if selected {
                                this.current_diff = Some(diff);
                                this.current_diff_syntax = syntax;
                            }
                        }
                        Err(error) => {
                            error!(
                                operation = "vcs_compare_diff_load",
                                repo_id = %repo,
                                path = %path_for_diff,
                                from = %from,
                                to = %to,
                                error = %error,
                                "compare diff failed"
                            );
                            let selected = this
                                .active_file_tab_idx
                                .and_then(|index| this.file_tabs.get(index))
                                .is_some_and(|tab| {
                                    tab.path == path_for_diff && tab.source == source_for_diff
                                });
                            if selected {
                                this.error = Some(format!("加载比较 Diff 失败：{error}"));
                            }
                        }
                    }
                    cx.notify();
                });
        })
        .detach();
    }
}
