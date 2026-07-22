//! hunk 级 patch：discard_hunk（按 source 分流回滚）+ build_patch_for_hunk

use gpui::Context;
use ramag_domain::entities::{DiffLineKind, FileChangeKind, FileDiff, MAX_GIT_PATCH_BYTES};
use tracing::{error, info};

use super::helpers::{FileTabSource, GroupKind};
use super::vcs_view::VcsView;

impl VcsView {
    /// 当前 active Changes tab 的分组；非 Changes diff 返回 None
    pub(super) fn active_changes_kind(&self) -> Option<GroupKind> {
        self.active_file_tab_idx
            .and_then(|i| self.file_tabs.get(i))
            .and_then(|t| match &t.source {
                FileTabSource::Changes(k) => Some(*k),
                _ => None,
            })
    }

    /// 当前 diff 是否 Staged 组（hunk 回滚按钮的 tooltip / 确认分流用）
    pub(super) fn active_changes_kind_is_staged(&self) -> bool {
        matches!(self.active_changes_kind(), Some(GroupKind::Staged))
    }

    /// 回滚 hunk：Unstaged 走 discard_patch（reverse 到 index）/ Staged 走 unstage_patch（reverse 撤回工作区）。
    /// 失败常因 diff 拉取后工作区或 index 又改过，patch 上下文不匹配
    pub(super) fn discard_hunk(&mut self, hunk_idx: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let Some(diff) = self.current_diff.clone() else {
            return;
        };
        // 仅 Changes 文件的 hunk 可回滚；其他来源（Commit detail / ProjectFiles）UI 不应渲染该按钮
        let Some(kind) = self.active_changes_kind() else {
            self.error = Some("当前不是 Changes diff，无法回滚".into());
            cx.notify();
            return;
        };
        if !matches!(kind, GroupKind::Staged | GroupKind::Unstaged) {
            // Untracked / Conflict diff 在 render_diff_body 里就被替换为 placeholder，
            // 不会渲染 hunk header，所以理论到不了这里；保险起见兜底
            self.error = Some("此类文件不支持 hunk 回滚".into());
            cx.notify();
            return;
        }
        let patch = match build_patch_for_hunk(&diff, hunk_idx) {
            Ok(patch) => patch,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let driver = self.driver.clone();
        let label = match kind {
            GroupKind::Staged => "移出暂存区中…",
            _ => "丢弃 hunk 中…",
        };
        if !self.begin_op(label, cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = match kind {
                GroupKind::Staged => driver.unstage_patch(&repo, &patch).await,
                GroupKind::Unstaged => driver.discard_patch(&repo, &patch).await,
                _ => unreachable!("已在前置分支拦截"),
            };
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.status(&repo).await,
                "workspace status",
            );
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(()) => {
                        info!(hunk_idx, ?kind, "hunk revert completed");
                        if let Some(s) = new_status {
                            this.status = Some(s);
                        }
                        // tabs 对齐：同文件两组 tab 缓存一并失效；变更全回滚则关 tab；active 自动重拉
                        this.sync_changes_tabs_with_status(cx);
                    }
                    Err(e) => {
                        error!(error = %e, hunk_idx, ?kind, "hunk revert failed");
                        let action = match kind {
                            GroupKind::Staged => "撤回 hunk 到工作区",
                            GroupKind::Unstaged => "回滚 hunk 到 index",
                            _ => "回滚 hunk",
                        };
                        this.error = Some(format!("{action} 失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 暂存单个 hunk（`git apply --cached`）：部分暂存的核心操作，仅未暂存 diff 可用
    pub(super) fn stage_hunk(&mut self, hunk_idx: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let Some(diff) = self.current_diff.clone() else {
            return;
        };
        if !matches!(self.active_changes_kind(), Some(GroupKind::Unstaged)) {
            self.error = Some("仅未暂存改动支持按 hunk 暂存".into());
            cx.notify();
            return;
        }
        let patch = match build_patch_for_hunk(&diff, hunk_idx) {
            Ok(patch) => patch,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let driver = self.driver.clone();
        if !self.begin_op("暂存 hunk 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.stage_patch(&repo, &patch).await;
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.status(&repo).await,
                "workspace status",
            );
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(()) => {
                        info!(hunk_idx, "hunk stage completed");
                        if let Some(s) = new_status {
                            this.status = Some(s);
                        }
                        this.sync_changes_tabs_with_status(cx);
                    }
                    Err(e) => {
                        error!(error = %e, hunk_idx, "hunk stage failed");
                        this.error = Some(format!("暂存 hunk 失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// 整个 hunk（含 context + `+/-` 全部行）→ unified diff patch，给 hunk 回滚用
pub(super) fn build_patch_for_hunk(diff: &FileDiff, hunk_idx: usize) -> Result<String, String> {
    let hunk = diff
        .hunks
        .get(hunk_idx)
        .ok_or_else(|| "hunk 索引越界".to_string())?;
    let mut out = String::new();
    let path = &diff.path;
    let old_path = diff.old_path.as_deref().unwrap_or(path);
    if path.chars().any(char::is_control) || old_path.chars().any(char::is_control) {
        return Err("文件路径包含控制字符，当前不支持行级操作；请改用整文件暂存或撤回".into());
    }
    append_patch_bounded(
        &mut out,
        &format!("diff --git a/{old_path} b/{path}\n"),
        MAX_GIT_PATCH_BYTES,
    )?;
    if matches!(
        diff.change_kind,
        FileChangeKind::Added | FileChangeKind::Untracked
    ) {
        append_patch_bounded(&mut out, "--- /dev/null\n", MAX_GIT_PATCH_BYTES)?;
    } else {
        append_patch_bounded(
            &mut out,
            &format!("--- a/{old_path}\n"),
            MAX_GIT_PATCH_BYTES,
        )?;
    }
    if matches!(diff.change_kind, FileChangeKind::Deleted) {
        append_patch_bounded(&mut out, "+++ /dev/null\n", MAX_GIT_PATCH_BYTES)?;
    } else {
        append_patch_bounded(&mut out, &format!("+++ b/{path}\n"), MAX_GIT_PATCH_BYTES)?;
    }
    append_patch_bounded(
        &mut out,
        &format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        ),
        MAX_GIT_PATCH_BYTES,
    )?;
    for line in &hunk.lines {
        let prefix = match line.kind {
            DiffLineKind::Context => " ",
            DiffLineKind::Add => "+",
            DiffLineKind::Delete => "-",
        };
        append_patch_bounded(&mut out, prefix, MAX_GIT_PATCH_BYTES)?;
        append_patch_bounded(&mut out, &line.text, MAX_GIT_PATCH_BYTES)?;
        append_patch_bounded(&mut out, "\n", MAX_GIT_PATCH_BYTES)?;
    }
    Ok(out)
}

fn append_patch_bounded(output: &mut String, value: &str, limit: usize) -> Result<(), String> {
    let next = output
        .len()
        .checked_add(value.len())
        .ok_or_else(|| "Git patch 长度溢出".to_string())?;
    if next > limit {
        return Err(format!(
            "Git patch 超过 {} MiB 安全上限，请缩小 hunk",
            MAX_GIT_PATCH_BYTES / 1024 / 1024
        ));
    }
    output.push_str(value);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use ramag_domain::entities::{DiffLine, FileChangeKind, Hunk};

    use super::*;

    fn diff(kind: FileChangeKind, old_start: u32, new_start: u32) -> FileDiff {
        FileDiff {
            path: "src/new.rs".into(),
            old_path: None,
            change_kind: kind,
            binary: false,
            old_mode: None,
            new_mode: None,
            hunks: vec![Hunk {
                old_start,
                old_lines: 0,
                new_start,
                new_lines: 1,
                heading: None,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Add,
                    old_lineno: None,
                    new_lineno: Some(new_start),
                    text: "line".into(),
                }],
            }],
        }
    }

    #[test]
    fn added_file_patch_uses_dev_null_and_real_range() {
        let patch = build_patch_for_hunk(&diff(FileChangeKind::Added, 0, 1), 0).unwrap();
        assert!(patch.contains("--- /dev/null\n+++ b/src/new.rs\n"));
        assert!(patch.contains("@@ -0,0 +1,1 @@"));
    }

    #[test]
    fn modified_file_patch_keeps_actual_hunk_position() {
        let mut modified = diff(FileChangeKind::Modified, 42, 43);
        modified.hunks[0].old_lines = 1;
        let patch = build_patch_for_hunk(&modified, 0).unwrap();
        assert!(patch.contains("--- a/src/new.rs\n+++ b/src/new.rs\n"));
        assert!(patch.contains("@@ -42,1 +43,1 @@"));
    }

    #[test]
    fn patch_builder_stops_before_exceeding_budget() {
        let mut output = String::new();
        assert!(append_patch_bounded(&mut output, "1234", 4).is_ok());
        assert!(append_patch_bounded(&mut output, "5", 4).is_err());
        assert_eq!(output, "1234");
    }

    #[test]
    fn patch_builder_rejects_paths_that_need_git_header_quoting() {
        let mut special = diff(FileChangeKind::Modified, 1, 1);
        special.path = "line\nbreak.rs".into();
        assert!(build_patch_for_hunk(&special, 0).is_err());
    }
}
