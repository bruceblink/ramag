//! VcsView 历史 ops：history 切换 + commit 详情 + 搜索解析。子模块 modify / blame_ops / reflog_ops

mod blame_ops;
mod modify;
mod reflog_ops;
#[cfg(test)]
mod tests;

use gpui::Context;
use ramag_domain::entities::DiffKind;
use tracing::error;

use super::helpers::{HistoryRefFilter, ViewMode};
use super::vcs_view::VcsView;

impl VcsView {
    /// 切换到提交历史前取消 reflog 请求，防止旧回包覆盖当前过滤范围。
    fn select_commit_history_scope(&mut self) {
        self.showing_reflog = false;
        self.loading_reflog = false;
        self.reflog_request_seq = self.reflog_request_seq.wrapping_add(1);
    }

    /// 历史列表只保留摘要；复制完整 message 时按需读取正文。
    pub(crate) fn copy_commit_message(&mut self, commit_id: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|repo| repo.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        cx.spawn(async move |this, cx| {
            let result = driver.commit_details(&repo, &commit_id).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) {
                    return;
                }
                match result {
                    Ok(commit) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            commit.message_full(),
                        ));
                        this.notify_success("已复制提交信息", cx);
                    }
                    Err(error) => {
                        tracing::error!(
                            operation = "vcs_commit_message_copy",
                            repo_id = %repo,
                            commit_id = %commit_id,
                            error = %error,
                            "copy commit message failed"
                        );
                        this.error = Some(format!("读取提交信息失败：{error}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 单文件历史：设 path_filter + 打开下半 history pane（history_pane_visible=true 必需，否则无反馈）
    pub(crate) fn view_file_history(&mut self, path: String, cx: &mut Context<Self>) {
        self.history_path_filter = Some(path);
        self.history_ref_filter = None;
        self.select_commit_history_scope();
        self.view_mode = ViewMode::History;
        self.history_pane_visible = true;
        self.set_history_commits(Vec::new());
        self.load_history_page(0, cx);
    }

    /// 清除单文件历史过滤，回到全仓库 history
    pub(crate) fn clear_history_path_filter(&mut self, cx: &mut Context<Self>) {
        self.history_path_filter = None;
        self.select_commit_history_scope();
        self.set_history_commits(Vec::new());
        self.load_history_page(0, cx);
    }

    /// 查看指定本地分支、远程分支或 tag 的提交历史。
    pub(in crate::views) fn view_ref_history(
        &mut self,
        filter: HistoryRefFilter,
        cx: &mut Context<Self>,
    ) {
        self.history_path_filter = None;
        self.history_ref_filter = Some(filter);
        self.select_commit_history_scope();
        self.view_mode = ViewMode::History;
        self.history_pane_visible = true;
        self.set_history_commits(Vec::new());
        self.load_history_page(0, cx);
    }

    /// 清除分支或 tag 历史过滤，回到当前 HEAD 的提交历史。
    pub(crate) fn clear_history_ref_filter(&mut self, cx: &mut Context<Self>) {
        self.history_ref_filter = None;
        self.select_commit_history_scope();
        self.set_history_commits(Vec::new());
        self.load_history_page(0, cx);
    }

    /// 触发 commit 搜索：解析 search_input + 重新拉首页
    pub(crate) fn apply_history_search(&mut self, cx: &mut Context<Self>) {
        self.set_history_commits(Vec::new());
        self.load_history_page(0, cx);
    }

    /// 进入「commit 详情视图」：并发按需读取正文与文件列表。
    pub(crate) fn load_commit_detail(&mut self, commit_id: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        self.capture_active_project_draft(cx);
        let driver = self.driver.clone();
        let commit = self
            .history_commits
            .iter()
            .find(|c| c.id.0 == commit_id)
            .cloned();
        self.viewing_commit = commit;
        self.reset_commit_files_tree();
        self.selected_commit_file = None;
        self.commit_file_diff = None;
        self.loading_commit_files = true;
        self.commit_detail_request_seq = self.commit_detail_request_seq.wrapping_add(1);
        let request_seq = self.commit_detail_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (details, files) = futures::future::join(
                driver.commit_details(&repo, &commit_id),
                driver.list_commit_files(&repo, &commit_id),
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.commit_detail_request_seq != request_seq {
                    return;
                }
                this.loading_commit_files = false;
                match details {
                    Ok(commit) => this.viewing_commit = Some(std::rc::Rc::new(commit)),
                    Err(e) => {
                        error!(
                            operation = "vcs_commit_detail_load",
                            repo_id = %repo,
                            commit_id = %commit_id,
                            error = %e,
                            "load commit details failed"
                        );
                        this.error = Some(format!("加载 commit 详情失败：{e}"));
                    }
                }
                match files {
                    Ok(files) => {
                        this.commit_files = std::rc::Rc::new(files);
                        this.commit_files_rows_cache.get_mut().take();
                    }
                    Err(e) => {
                        error!(
                            operation = "vcs_commit_files_load",
                            repo_id = %repo,
                            commit_id = %commit_id,
                            error = %e,
                            "load commit files failed"
                        );
                        this.error = Some(format!("加载 commit 文件列表失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 点选 commit 文件 → 创建 / 复用 file_tab + 拉 commit-vs-parent diff，主区与 Changes 统一
    pub(crate) fn select_commit_file(
        &mut self,
        path: String,
        commit_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        self.capture_active_project_draft(cx);
        let change_kind = self
            .commit_files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.staged);
        let source = super::helpers::FileTabSource::Commit {
            commit_id: commit_id.clone(),
            change_kind,
        };
        let existing = self.file_tabs.iter().position(|t| {
            t.path == path
                && matches!(
                    &t.source,
                    super::helpers::FileTabSource::Commit {
                        commit_id: existing_commit,
                        ..
                    } if existing_commit == &commit_id
                )
        });
        let is_new_tab = existing.is_none();
        let idx = existing.unwrap_or_else(|| {
            self.file_tabs.push(super::helpers::FileTab {
                path: path.clone(),
                source: source.clone(),
                cached_diff: None,
                cached_diff_syntax: None,
                cached_content: None,
            });
            self.file_tabs.len() - 1
        });
        if is_new_tab {
            self.scroll_file_tabs_to_end();
        }
        // 文件类型信息可能在恢复 session 后才重新加载；复用 tab 时补齐最新值。
        self.file_tabs[idx].source = source.clone();
        let same_target = self.active_file_tab_idx == Some(idx)
            && self.selected_commit_file.as_deref() == Some(path.as_str());
        if !same_target {
            self.reset_blame_context();
            self.expanded_diff_spacers.clear();
            self.diff_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
            self.diff_h_scroll
                .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
            self.diff_scroll_gesture.reset();
        }
        self.diff_request_seq = self.diff_request_seq.wrapping_add(1);
        let request_seq = self.diff_request_seq;
        self.active_file_tab_idx = Some(idx);
        self.selected_commit_file = Some(path.clone());
        self.selected_file = None;
        self.selected_pf_path = None;
        self.current_file_content = None;
        if let Some(cached) = self.file_tabs[idx].cached_diff.clone() {
            self.current_diff = Some(cached.clone());
            self.current_diff_syntax = self.file_tabs[idx].cached_diff_syntax.clone();
            self.commit_file_diff = Some(cached);
            self.loading_diff = false;
            cx.notify();
            return;
        }
        self.current_diff = None;
        self.current_diff_syntax = None;
        self.commit_file_diff = None;
        self.loading_diff = true;
        cx.notify();

        let driver = self.driver.clone();
        let context_lines = self.diff_view_mode.context_lines();
        let path_for_diff = path.clone();
        let commit_for_diff = commit_id.clone();
        let source_for_diff = source;
        cx.spawn(async move |this, cx| {
            let result = driver
                .diff_file_full_opts(
                    &repo,
                    &path_for_diff,
                    DiffKind::CommitVsParent(ramag_domain::entities::CommitId(commit_for_diff)),
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
                        Ok((d, syntax)) => {
                            let d = std::rc::Rc::new(d);
                            let syntax = syntax.map(std::rc::Rc::new);
                            let still_current = this.active_file_tab_idx.is_some_and(|idx| {
                                this.file_tabs.get(idx).is_some_and(|tab| {
                                    tab.path == path_for_diff && tab.source == source_for_diff
                                })
                            });
                            if still_current {
                                this.current_diff = Some(d.clone());
                                this.current_diff_syntax = syntax.clone();
                                this.commit_file_diff = Some(d.clone());
                            }
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
                                operation = "vcs_commit_diff_load",
                                repo_id = %repo,
                                commit_id = %commit_id,
                                path = %path_for_diff,
                                error = %e,
                                "commit diff failed"
                            );
                            if this.active_file_tab_idx.is_some_and(|idx| {
                                this.file_tabs.get(idx).is_some_and(|tab| {
                                    tab.path == path_for_diff && tab.source == source_for_diff
                                })
                            }) {
                                this.error = Some(format!("拉取 commit diff 失败：{e}"));
                            }
                        }
                    }
                    cx.notify();
                });
        })
        .detach();
    }

    /// 退出 commit 详情视图，回到 history 列表
    pub(crate) fn close_commit_detail(&mut self, cx: &mut Context<Self>) {
        self.commit_detail_request_seq = self.commit_detail_request_seq.wrapping_add(1);
        self.viewing_commit = None;
        self.reset_commit_files_tree();
        self.selected_commit_file = None;
        self.commit_file_diff = None;
        self.loading_commit_files = false;
        cx.notify();
    }
}

/// 解析搜索框 → (grep, author, since)：`@xxx`=author / `7d|1w|2m|12h|3y`=since / 其他=grep
pub(crate) fn parse_search_query(q: &str) -> (Option<String>, Option<String>, Option<String>) {
    if q.is_empty() {
        return (None, None, None);
    }
    let mut grep_parts: Vec<String> = Vec::new();
    let mut author: Option<String> = None;
    let mut since: Option<String> = None;
    for tok in q.split_whitespace() {
        if let Some(name) = tok.strip_prefix('@')
            && !name.is_empty()
        {
            author = Some(name.to_string());
            continue;
        }
        if let Some(s) = parse_relative_time(tok) {
            since = Some(s);
            continue;
        }
        grep_parts.push(tok.to_string());
    }
    let grep = if grep_parts.is_empty() {
        None
    } else {
        Some(grep_parts.join(" "))
    };
    (grep, author, since)
}

/// 把 `7d` / `1w` / `2m` / `12h` / `3y` 转成 git --since 接受的字符串
pub(crate) fn parse_relative_time(s: &str) -> Option<String> {
    if s.len() < 2 {
        return None;
    }
    let (num_part, unit) = s.split_at(s.len() - 1);
    let n: u32 = num_part.parse().ok()?;
    let unit_word = match unit {
        "h" => "hours",
        "d" => "days",
        "w" => "weeks",
        "m" => "months",
        "y" => "years",
        _ => return None,
    };
    Some(format!("{n} {unit_word} ago"))
}
