mod open;
mod remote_ops;
mod repository_ops;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::mapref::entry::Entry;

use ramag_domain::entities::{
    BlameLine, Branch, BranchKind, Commit, CommitId, ConflictContent, DiffKind, FileDiff,
    FileStatus, LogOptions, RebaseTodo, ReflogEntry, Remote, RepoConfig, RepoId, ResetKind, Stash,
    Tag, WorkingTreeStatus,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::GitDriver;

use crate::handle::{OpenRepo, run_write_blocking};
use crate::runtime::run_blocking;
use crate::{
    GitDriverImpl, RepoConfigExt, blame, cherry_pick, clone, commit_files, commit_op,
    conflict_content, conflict_ops, diff, errors, git_cmd, history_ops, log, merge, patch,
    rebase_interactive, reflog, remote, stash, status, tag, work_ops,
};

#[async_trait]
impl GitDriver for GitDriverImpl {
    fn name(&self) -> &'static str {
        "system-git"
    }

    async fn open_repo(&self, path: &Path) -> Result<RepoConfig> {
        open::open_repo(self, path).await
    }

    async fn close_repo(&self, repo: &RepoId) -> Result<()> {
        let Some(handle) = self.repos.get(repo).map(|entry| entry.clone()) else {
            return Ok(());
        };
        let path = handle.path.clone();
        let open_lock = self.open_lock(&path);
        let _guard = open_lock.lock().await;

        if self.repos.remove(repo).is_some()
            && let Entry::Occupied(entry) = self.by_path.entry(path)
            && entry.get() == repo
        {
            entry.remove();
        }
        Ok(())
    }

    async fn status(&self, repo: &RepoId) -> Result<WorkingTreeStatus> {
        let handle = self.get_repo(repo)?;
        run_blocking(move || status::collect_status(&handle.path, &handle.git_dir)).await
    }

    async fn status_paths(&self, repo: &RepoId, paths: &[String]) -> Result<Vec<FileStatus>> {
        let handle = self.get_repo(repo)?;
        let paths = paths.to_vec();
        run_blocking(move || status::collect_status_paths(&handle.path, &paths)).await
    }

    async fn list_branches(&self, repo: &RepoId, kind: BranchKind) -> Result<Vec<Branch>> {
        let handle = self.get_repo(repo)?;
        run_blocking(move || status::list_branches(&handle.path, kind)).await
    }

    async fn list_all_branches(&self, repo: &RepoId) -> Result<(Vec<Branch>, Vec<Branch>)> {
        let handle = self.get_repo(repo)?;
        run_blocking(move || status::list_all_branches(&handle.path)).await
    }

    async fn log(&self, repo: &RepoId, opts: LogOptions) -> Result<Vec<Commit>> {
        let handle = self.get_repo(repo)?;
        run_blocking(move || log::run_log_paged(&handle.path, &handle.log_pager, &opts)).await
    }

    async fn commit_details(&self, repo: &RepoId, revision: &str) -> Result<Commit> {
        git_cmd::validate_positional_arg(revision, "commit 详情 revision")?;
        let handle = self.get_repo(repo)?;
        let revision = revision.to_string();
        run_blocking(move || log::run_commit(&handle.path, &revision)).await
    }

    async fn diff_file(&self, repo: &RepoId, path: &str, kind: DiffKind) -> Result<FileDiff> {
        git_cmd::validate_path_arg(path, "diff 文件路径")?;
        let handle = self.get_repo(repo)?;
        let path = path.to_string();
        run_blocking(move || diff::run_diff(&handle.path, &path, &kind)).await
    }

    async fn diff_file_opts(
        &self,
        repo: &RepoId,
        path: &str,
        kind: DiffKind,
        ignore_whitespace: bool,
    ) -> Result<FileDiff> {
        git_cmd::validate_path_arg(path, "diff 文件路径")?;
        let handle = self.get_repo(repo)?;
        let path = path.to_string();
        run_blocking(move || diff::run_diff_opts(&handle.path, &path, &kind, ignore_whitespace))
            .await
    }

    async fn diff_file_full_opts(
        &self,
        repo: &RepoId,
        path: &str,
        kind: DiffKind,
        ignore_whitespace: bool,
        context_lines: u32,
    ) -> Result<FileDiff> {
        git_cmd::validate_path_arg(path, "diff 文件路径")?;
        let handle = self.get_repo(repo)?;
        let path = path.to_string();
        run_blocking(move || {
            diff::run_diff_full_opts(&handle.path, &path, &kind, ignore_whitespace, context_lines)
        })
        .await
    }

    // gix 写入 API 尚不稳定，写操作使用 Git 子进程。

    async fn stage(&self, repo: &RepoId, paths: &[String]) -> Result<()> {
        git_cmd::validate_path_args(paths, "待暂存文件列表")?;
        let handle = self.get_repo(repo)?;
        let paths = paths.to_vec();
        run_write_blocking(handle, move |p| work_ops::stage(p, &paths)).await
    }

    async fn unstage(&self, repo: &RepoId, paths: &[String]) -> Result<()> {
        git_cmd::validate_path_args(paths, "待撤回暂存文件列表")?;
        let handle = self.get_repo(repo)?;
        let paths = paths.to_vec();
        run_write_blocking(handle, move |p| work_ops::unstage(p, &paths)).await
    }

    async fn discard(&self, repo: &RepoId, paths: &[String]) -> Result<()> {
        git_cmd::validate_path_args(paths, "待丢弃文件列表")?;
        let handle = self.get_repo(repo)?;
        let paths = paths.to_vec();
        run_write_blocking(handle, move |p| work_ops::discard(p, &paths)).await
    }

    async fn list_files(&self, repo: &RepoId) -> Result<Vec<String>> {
        let handle = self.get_repo(repo)?;
        run_blocking(move || work_ops::list_files(&handle.path)).await
    }

    async fn list_files_paths(&self, repo: &RepoId, paths: &[String]) -> Result<Vec<String>> {
        let handle = self.get_repo(repo)?;
        let paths = paths.to_vec();
        run_blocking(move || work_ops::list_files_paths(&handle.path, &paths)).await
    }

    async fn commit(
        &self,
        repo: &RepoId,
        message: &str,
        amend: bool,
        sign: bool,
    ) -> Result<CommitId> {
        commit_op::validate_message(message)?;
        let handle = self.get_repo(repo)?;
        let message = message.to_string();
        run_write_blocking(handle, move |p| commit_op::run(p, &message, amend, sign)).await
    }

    async fn checkout(&self, repo: &RepoId, target: &str) -> Result<()> {
        git_cmd::validate_positional_arg(target, "checkout 目标")?;
        let handle = self.get_repo(repo)?;
        let target = target.to_string();
        run_write_blocking(handle, move |p| work_ops::checkout(p, &target)).await
    }

    async fn create_branch(&self, repo: &RepoId, name: &str, base: Option<&str>) -> Result<()> {
        git_cmd::validate_name_arg(name, "分支名")?;
        if let Some(base) = base {
            git_cmd::validate_positional_arg(base, "分支基点")?;
        }
        let handle = self.get_repo(repo)?;
        let name = name.to_string();
        let base = base.map(str::to_owned);
        run_write_blocking(handle, move |p| {
            work_ops::create_branch(p, &name, base.as_deref())
        })
        .await
    }

    async fn delete_branch(&self, repo: &RepoId, name: &str, force: bool) -> Result<()> {
        git_cmd::validate_name_arg(name, "分支名")?;
        let handle = self.get_repo(repo)?;
        let name = name.to_string();
        run_write_blocking(handle, move |p| work_ops::delete_branch(p, &name, force)).await
    }

    async fn fetch(&self, repo: &RepoId, remote: &str) -> Result<()> {
        remote_ops::fetch(self, repo, remote).await
    }

    async fn push(
        &self,
        repo: &RepoId,
        remote: &str,
        branch: &str,
        set_upstream: bool,
        force_with_lease: bool,
    ) -> Result<()> {
        remote_ops::push(self, repo, remote, branch, set_upstream, force_with_lease).await
    }

    async fn pull(&self, repo: &RepoId, remote: &str, branch: &str, rebase: bool) -> Result<()> {
        remote_ops::pull(self, repo, remote, branch, rebase).await
    }

    async fn fetch_streaming(
        &self,
        repo: &RepoId,
        remote: &str,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> Result<()> {
        remote_ops::fetch_streaming(self, repo, remote, cancel, progress).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn push_streaming(
        &self,
        repo: &RepoId,
        remote: &str,
        branch: &str,
        set_upstream: bool,
        force_with_lease: bool,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> Result<()> {
        remote_ops::push_streaming(
            self,
            repo,
            remote,
            branch,
            set_upstream,
            force_with_lease,
            cancel,
            progress,
        )
        .await
    }

    async fn pull_streaming(
        &self,
        repo: &RepoId,
        remote: &str,
        branch: &str,
        rebase: bool,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> Result<()> {
        remote_ops::pull_streaming(self, repo, remote, branch, rebase, cancel, progress).await
    }
    async fn list_stashes(&self, repo: &RepoId) -> Result<Vec<Stash>> {
        repository_ops::list_stashes(self, repo).await
    }

    async fn stash_save(
        &self,
        repo: &RepoId,
        message: Option<&str>,
        include_untracked: bool,
    ) -> Result<()> {
        repository_ops::stash_save(self, repo, message, include_untracked).await
    }

    async fn stash_apply(&self, repo: &RepoId, idx: usize, pop: bool) -> Result<()> {
        repository_ops::stash_apply(self, repo, idx, pop).await
    }

    async fn stash_drop(&self, repo: &RepoId, idx: usize) -> Result<()> {
        repository_ops::stash_drop(self, repo, idx).await
    }

    async fn list_tags(&self, repo: &RepoId) -> Result<Vec<Tag>> {
        repository_ops::list_tags(self, repo).await
    }

    async fn create_tag(
        &self,
        repo: &RepoId,
        name: &str,
        target: Option<&str>,
        message: Option<&str>,
        sign: bool,
    ) -> Result<()> {
        repository_ops::create_tag(self, repo, name, target, message, sign).await
    }

    async fn delete_tag(&self, repo: &RepoId, name: &str) -> Result<()> {
        repository_ops::delete_tag(self, repo, name).await
    }

    async fn push_tag(&self, repo: &RepoId, remote: &str, name: &str) -> Result<()> {
        repository_ops::push_tag(self, repo, remote, name).await
    }

    async fn stage_patch(&self, repo: &RepoId, patch: &str) -> Result<()> {
        patch::validate_patch(patch)?;
        let handle = self.get_repo(repo)?;
        let patch = patch.to_string();
        run_write_blocking(handle, move |p| patch::stage(p, &patch)).await
    }

    async fn unstage_patch(&self, repo: &RepoId, patch: &str) -> Result<()> {
        patch::validate_patch(patch)?;
        let handle = self.get_repo(repo)?;
        let patch = patch.to_string();
        run_write_blocking(handle, move |p| patch::unstage(p, &patch)).await
    }

    async fn discard_patch(&self, repo: &RepoId, patch: &str) -> Result<()> {
        patch::validate_patch(patch)?;
        let handle = self.get_repo(repo)?;
        let patch = patch.to_string();
        run_write_blocking(handle, move |p| patch::discard(p, &patch)).await
    }

    async fn merge(
        &self,
        repo: &RepoId,
        branch: &str,
        no_ff: bool,
        ff_only: bool,
        message: Option<&str>,
    ) -> Result<()> {
        git_cmd::validate_name_arg(branch, "合并分支名")?;
        if let Some(message) = message {
            merge::validate_message(message)?;
        }
        let handle = self.get_repo(repo)?;
        let branch = branch.to_string();
        let message = message.map(str::to_owned);
        run_write_blocking(handle, move |p| {
            merge::start(p, &branch, no_ff, ff_only, message.as_deref())
        })
        .await
    }

    async fn merge_abort(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, merge::abort).await
    }

    async fn merge_continue(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, merge::cont).await
    }

    async fn cherry_pick(&self, repo: &RepoId, commit: &str) -> Result<()> {
        git_cmd::validate_positional_arg(commit, "cherry-pick commit")?;
        let handle = self.get_repo(repo)?;
        let commit = commit.to_string();
        run_write_blocking(handle, move |p| cherry_pick::start(p, &commit)).await
    }

    async fn cherry_pick_abort(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, cherry_pick::abort).await
    }

    async fn cherry_pick_continue(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, cherry_pick::cont).await
    }

    async fn use_ours(&self, repo: &RepoId, paths: &[String]) -> Result<()> {
        git_cmd::validate_path_args(paths, "冲突文件列表")?;
        let handle = self.get_repo(repo)?;
        let paths = paths.to_vec();
        run_write_blocking(handle, move |p| conflict_ops::use_ours(p, &paths)).await
    }

    async fn use_theirs(&self, repo: &RepoId, paths: &[String]) -> Result<()> {
        git_cmd::validate_path_args(paths, "冲突文件列表")?;
        let handle = self.get_repo(repo)?;
        let paths = paths.to_vec();
        run_write_blocking(handle, move |p| conflict_ops::use_theirs(p, &paths)).await
    }

    async fn reset(&self, repo: &RepoId, target: &str, kind: ResetKind) -> Result<()> {
        git_cmd::validate_positional_arg(target, "reset 目标")?;
        let handle = self.get_repo(repo)?;
        let target = target.to_string();
        run_write_blocking(handle, move |p| history_ops::reset(p, &target, kind)).await
    }

    async fn revert(&self, repo: &RepoId, commit: &str) -> Result<()> {
        git_cmd::validate_positional_arg(commit, "revert commit")?;
        let handle = self.get_repo(repo)?;
        let commit = commit.to_string();
        run_write_blocking(handle, move |p| history_ops::revert(p, &commit)).await
    }

    async fn revert_abort(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, history_ops::revert_abort).await
    }

    async fn revert_continue(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, history_ops::revert_continue).await
    }

    async fn rebase(&self, repo: &RepoId, onto: &str) -> Result<()> {
        git_cmd::validate_positional_arg(onto, "rebase 上游")?;
        let handle = self.get_repo(repo)?;
        let onto = onto.to_string();
        run_write_blocking(handle, move |p| {
            git_cmd::validate_positional_arg(&onto, "rebase 上游")?;
            git_cmd::run_git_bytes(p, &["rebase", &onto]).map(|_| ())
        })
        .await
    }

    async fn rebase_continue(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, move |p| {
            git_cmd::run_git_bytes(p, &["rebase", "--continue"]).map(|_| ())
        })
        .await
    }

    async fn rebase_skip(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, move |p| {
            git_cmd::run_git_bytes(p, &["rebase", "--skip"]).map(|_| ())
        })
        .await
    }

    async fn rebase_abort(&self, repo: &RepoId) -> Result<()> {
        let handle = self.get_repo(repo)?;
        run_write_blocking(handle, move |p| {
            git_cmd::run_git_bytes(p, &["rebase", "--abort"]).map(|_| ())
        })
        .await
    }

    async fn list_remotes(&self, repo: &RepoId) -> Result<Vec<Remote>> {
        let handle = self.get_repo(repo)?;
        run_blocking(move || remote::list(&handle.path)).await
    }

    async fn add_remote(&self, repo: &RepoId, name: &str, url: &str) -> Result<()> {
        git_cmd::validate_name_arg(name, "远程名")?;
        git_cmd::validate_positional_arg(url, "远程 URL")?;
        let handle = self.get_repo(repo)?;
        let name = name.to_string();
        let url = url.to_string();
        run_write_blocking(handle, move |p| remote::add(p, &name, &url)).await
    }

    async fn remove_remote(&self, repo: &RepoId, name: &str) -> Result<()> {
        git_cmd::validate_name_arg(name, "远程名")?;
        let handle = self.get_repo(repo)?;
        let name = name.to_string();
        run_write_blocking(handle, move |p| remote::remove(p, &name)).await
    }

    async fn set_remote_url(&self, repo: &RepoId, name: &str, url: &str) -> Result<()> {
        git_cmd::validate_name_arg(name, "远程名")?;
        git_cmd::validate_positional_arg(url, "远程 URL")?;
        let handle = self.get_repo(repo)?;
        let name = name.to_string();
        let url = url.to_string();
        run_write_blocking(handle, move |p| remote::set_url(p, &name, &url)).await
    }

    async fn rename_remote(&self, repo: &RepoId, old: &str, new: &str) -> Result<()> {
        git_cmd::validate_name_arg(old, "原远程名")?;
        git_cmd::validate_name_arg(new, "新远程名")?;
        let handle = self.get_repo(repo)?;
        let old = old.to_string();
        let new = new.to_string();
        run_write_blocking(handle, move |p| remote::rename(p, &old, &new)).await
    }

    async fn list_commit_files(&self, repo: &RepoId, commit: &str) -> Result<Vec<FileStatus>> {
        git_cmd::validate_positional_arg(commit, "commit id")?;
        let handle = self.get_repo(repo)?;
        let commit = commit.to_string();
        run_blocking(move || commit_files::list(&handle.path, &commit)).await
    }

    async fn blame(&self, repo: &RepoId, path: &str) -> Result<Vec<BlameLine>> {
        git_cmd::validate_path_arg(path, "blame 文件路径")?;
        let handle = self.get_repo(repo)?;
        let path = path.to_string();
        run_blocking(move || blame::run(&handle.path, &path)).await
    }

    async fn list_reflog(
        &self,
        repo: &RepoId,
        ref_name: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<ReflogEntry>> {
        if let Some(ref_name) = ref_name {
            git_cmd::validate_positional_arg(ref_name, "reflog 引用")?;
        }
        if limit.is_some_and(|limit| limit > git_cmd::MAX_PARSED_GIT_ITEMS) {
            return Err(DomainError::InvalidConfig(format!(
                "reflog 数量超过 {} 条安全上限",
                git_cmd::MAX_PARSED_GIT_ITEMS
            )));
        }
        let handle = self.get_repo(repo)?;
        let ref_name = ref_name.map(str::to_owned);
        run_blocking(move || reflog::list(&handle.path, ref_name.as_deref(), limit)).await
    }

    async fn clone_repo(&self, url: &str, dest: &Path) -> Result<RepoConfig> {
        git_cmd::validate_positional_arg(url, "仓库 URL")?;
        let url = url.to_string();
        let dest_clone = dest.to_path_buf();
        run_blocking(move || clone::clone_repo(&url, &dest_clone)).await?;
        self.open_repo(dest).await
    }

    async fn clone_repo_streaming(
        &self,
        url: &str,
        dest: &Path,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> Result<RepoConfig> {
        git_cmd::validate_positional_arg(url, "仓库 URL")?;
        let url = url.to_string();
        let dest_clone = dest.to_path_buf();
        run_blocking(move || clone::clone_repo_streaming(&url, &dest_clone, cancel, progress))
            .await?;
        self.open_repo(dest).await
    }

    async fn init_repo(&self, path: &Path) -> Result<RepoConfig> {
        let path_init = path.to_path_buf();
        run_blocking(move || clone::init_repo(&path_init)).await?;
        self.open_repo(path).await
    }

    async fn interactive_rebase_plan(&self, repo: &RepoId, onto: &str) -> Result<Vec<RebaseTodo>> {
        git_cmd::validate_positional_arg(onto, "rebase 上游引用")?;
        let handle = self.get_repo(repo)?;
        let onto = onto.to_string();
        run_blocking(move || rebase_interactive::plan(&handle.path, &onto)).await
    }

    async fn interactive_rebase_execute(
        &self,
        repo: &RepoId,
        onto: &str,
        todos: &[RebaseTodo],
    ) -> Result<()> {
        git_cmd::validate_positional_arg(onto, "rebase 上游引用")?;
        rebase_interactive::validate_todos(todos)?;
        let handle = self.get_repo(repo)?;
        let onto = onto.to_string();
        let todos = todos.to_vec();
        run_write_blocking(handle, move |p| {
            rebase_interactive::execute(p, &onto, &todos)
        })
        .await
    }

    async fn get_conflict_content(&self, repo: &RepoId, path: &str) -> Result<ConflictContent> {
        git_cmd::validate_path_arg(path, "冲突文件路径")?;
        let handle = self.get_repo(repo)?;
        let path = path.to_string();
        run_blocking(move || conflict_content::get_content(&handle.path, &path)).await
    }
}
