//! GitDriver trait：Git 操作统一抽象，与 SQL Driver / KvDriver 并列。
//! dyn-safe；底层按 RepoId 缓存仓库路径与写锁；同步 API 经 worker pool 桥接异步

use std::path::Path;

use async_trait::async_trait;

use crate::entities::{
    BlameLine, Branch, BranchKind, Commit, ConflictContent, FileDiff, FileStatus, LogOptions,
    RebaseTodo, ReflogEntry, Remote, RepoConfig, RepoId, ResetKind, Stash, Tag, WorkingTreeStatus,
};
use crate::error::{DomainError, Result};

fn not_impl<T>(method: &'static str) -> Result<T> {
    Err(DomainError::NotImplemented(method.into()))
}

#[async_trait]
pub trait GitDriver: Send + Sync {
    /// 驱动名称，如 "system-git"
    fn name(&self) -> &'static str;

    /// 打开本地仓库目录（含 `.git`）
    async fn open_repo(&self, path: &Path) -> Result<RepoConfig>;

    /// 释放底层句柄；ramag 侧的 RepoConfig 不受影响
    async fn close_repo(&self, repo: &RepoId) -> Result<()>;

    /// 工作区状态：HEAD / 变更文件 / ahead-behind / 进行中操作
    async fn status(&self, repo: &RepoId) -> Result<WorkingTreeStatus>;

    /// 只查询指定路径前缀的文件状态；供文件监听增量刷新，默认实现从完整状态中过滤。
    async fn status_paths(&self, repo: &RepoId, paths: &[String]) -> Result<Vec<FileStatus>> {
        if paths.is_empty() {
            return Err(DomainError::InvalidConfig("增量状态路径不能为空".into()));
        }
        let status = self.status(repo).await?;
        Ok(status
            .files
            .into_iter()
            .filter(|file| {
                paths.iter().any(|prefix| {
                    path_matches_prefix(&file.path, prefix)
                        || file
                            .old_path
                            .as_deref()
                            .is_some_and(|path| path_matches_prefix(path, prefix))
                })
            })
            .collect())
    }

    async fn list_branches(&self, repo: &RepoId, kind: BranchKind) -> Result<Vec<Branch>>;

    /// 一次读取本地与远程分支；高性能实现可合并为单次后端查询。
    async fn list_all_branches(&self, repo: &RepoId) -> Result<(Vec<Branch>, Vec<Branch>)> {
        let local = self.list_branches(repo, BranchKind::Local).await?;
        let remote = self.list_branches(repo, BranchKind::Remote).await?;
        Ok((local, remote))
    }

    /// 提交日志，分页通过 LogOptions::skip
    async fn log(&self, repo: &RepoId, opts: LogOptions) -> Result<Vec<Commit>>;

    /// 按需读取单个 commit 的完整正文；历史列表实现可只返回摘要以降低首屏开销。
    async fn commit_details(&self, repo: &RepoId, revision: &str) -> Result<Commit> {
        let commits = self
            .log(
                repo,
                LogOptions {
                    start: Some(revision.to_string()),
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .await?;
        commits
            .into_iter()
            .next()
            .ok_or_else(|| DomainError::QueryFailed(format!("未找到 commit：{revision}")))
    }

    /// 单文件 diff，`kind` 控制对比来源
    async fn diff_file(
        &self,
        repo: &RepoId,
        path: &str,
        kind: crate::entities::DiffKind,
    ) -> Result<FileDiff>;

    /// 带 ignore_whitespace 的 diff；默认退化到 `diff_file`
    async fn diff_file_opts(
        &self,
        repo: &RepoId,
        path: &str,
        kind: crate::entities::DiffKind,
        _ignore_whitespace: bool,
    ) -> Result<FileDiff> {
        self.diff_file(repo, path, kind).await
    }

    /// 带 ignore_whitespace + 自定义上下文行数。`context_lines`：3=标准、0=仅变更行、999_999=全文件
    async fn diff_file_full_opts(
        &self,
        repo: &RepoId,
        path: &str,
        kind: crate::entities::DiffKind,
        ignore_whitespace: bool,
        _context_lines: u32,
    ) -> Result<FileDiff> {
        self.diff_file_opts(repo, path, kind, ignore_whitespace)
            .await
    }

    /// 加入暂存区
    async fn stage(&self, _repo: &RepoId, _paths: &[String]) -> Result<()> {
        not_impl("stage")
    }

    /// 暂存区撤回
    async fn unstage(&self, _repo: &RepoId, _paths: &[String]) -> Result<()> {
        not_impl("unstage")
    }

    /// 丢弃工作区改动（`git checkout -- <path>`）
    async fn discard(&self, _repo: &RepoId, _paths: &[String]) -> Result<()> {
        not_impl("discard")
    }

    /// 创建 commit。amend=修改上一次，sign=GPG 签名
    async fn commit(
        &self,
        _repo: &RepoId,
        _message: &str,
        _amend: bool,
        _sign: bool,
    ) -> Result<crate::entities::CommitId> {
        not_impl("commit")
    }

    /// 切换到分支 / commit / tag
    async fn checkout(&self, _repo: &RepoId, _target: &str) -> Result<()> {
        not_impl("checkout")
    }

    /// 创建本地分支，base=None 时基于当前 HEAD
    async fn create_branch(&self, _repo: &RepoId, _name: &str, _base: Option<&str>) -> Result<()> {
        not_impl("create_branch")
    }

    /// 删除本地分支，force=true 才允许删未合并的
    async fn delete_branch(&self, _repo: &RepoId, _name: &str, _force: bool) -> Result<()> {
        not_impl("delete_branch")
    }

    /// fetch（不合并）
    async fn fetch(&self, _repo: &RepoId, _remote: &str) -> Result<()> {
        not_impl("fetch")
    }

    /// push。set_upstream=`-u`，force_with_lease=`--force-with-lease`
    async fn push(
        &self,
        _repo: &RepoId,
        _remote: &str,
        _branch: &str,
        _set_upstream: bool,
        _force_with_lease: bool,
    ) -> Result<()> {
        not_impl("push")
    }

    /// fetch + merge / rebase 当前分支
    async fn pull(
        &self,
        _repo: &RepoId,
        _remote: &str,
        _branch: &str,
        _rebase: bool,
    ) -> Result<()> {
        not_impl("pull")
    }

    // —— 流式变体（带进度 + 可取消）：与 clone_repo_streaming 同哲学。默认回退到无进度版 ——

    /// Fetch（带进度 + 可取消）
    async fn fetch_streaming(
        &self,
        repo: &RepoId,
        remote: &str,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> Result<()> {
        let _ = (cancel, progress);
        self.fetch(repo, remote).await
    }

    /// Push（带进度 + 可取消）
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
        let _ = (cancel, progress);
        self.push(repo, remote, branch, set_upstream, force_with_lease)
            .await
    }

    /// Pull（带进度 + 可取消）
    async fn pull_streaming(
        &self,
        repo: &RepoId,
        remote: &str,
        branch: &str,
        rebase: bool,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> Result<()> {
        let _ = (cancel, progress);
        self.pull(repo, remote, branch, rebase).await
    }

    async fn list_stashes(&self, _repo: &RepoId) -> Result<Vec<Stash>> {
        not_impl("list_stashes")
    }

    /// 列出 git 跟踪 + 未跟踪但未 ignore 的相对路径，等价 `git ls-files --cached --others --exclude-standard`
    async fn list_files(&self, _repo: &RepoId) -> Result<Vec<String>> {
        not_impl("list_files")
    }

    /// 只重查指定路径前缀在 Project Files 中的成员；默认实现从全量列表过滤。
    async fn list_files_paths(&self, repo: &RepoId, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Err(DomainError::InvalidConfig(
                "增量 Project Files 路径不能为空".into(),
            ));
        }
        Ok(self
            .list_files(repo)
            .await?
            .into_iter()
            .filter(|file| paths.iter().any(|prefix| path_matches_prefix(file, prefix)))
            .collect())
    }

    async fn stash_save(
        &self,
        _repo: &RepoId,
        _message: Option<&str>,
        _include_untracked: bool,
    ) -> Result<()> {
        not_impl("stash_save")
    }

    /// pop=true 应用后删除
    async fn stash_apply(&self, _repo: &RepoId, _idx: usize, _pop: bool) -> Result<()> {
        not_impl("stash_apply")
    }

    async fn stash_drop(&self, _repo: &RepoId, _idx: usize) -> Result<()> {
        not_impl("stash_drop")
    }

    /// 含轻量 + annotated
    async fn list_tags(&self, _repo: &RepoId) -> Result<Vec<Tag>> {
        not_impl("list_tags")
    }

    /// target=None 基于 HEAD；message=Some 走 annotated；sign 隐含 annotated
    async fn create_tag(
        &self,
        _repo: &RepoId,
        _name: &str,
        _target: Option<&str>,
        _message: Option<&str>,
        _sign: bool,
    ) -> Result<()> {
        not_impl("create_tag")
    }

    async fn delete_tag(&self, _repo: &RepoId, _name: &str) -> Result<()> {
        not_impl("delete_tag")
    }

    async fn push_tag(&self, _repo: &RepoId, _remote: &str, _name: &str) -> Result<()> {
        not_impl("push_tag")
    }

    /// 写 patch 进暂存区。实现走 `git apply --cached --recount -`
    async fn stage_patch(&self, _repo: &RepoId, _patch: &str) -> Result<()> {
        not_impl("stage_patch")
    }

    /// 反向：从暂存区撤回 patch（不影响工作区）
    async fn unstage_patch(&self, _repo: &RepoId, _patch: &str) -> Result<()> {
        not_impl("unstage_patch")
    }

    /// hunk 级回滚到 HEAD，仅回滚指定 patch，保留其他改动
    async fn discard_patch(&self, _repo: &RepoId, _patch: &str) -> Result<()> {
        not_impl("discard_patch")
    }

    /// 合并分支到 HEAD。no_ff/ff_only 互斥；都 false 走 git 默认；冲突时进入 merge 进行中
    async fn merge(
        &self,
        _repo: &RepoId,
        _branch: &str,
        _no_ff: bool,
        _ff_only: bool,
        _message: Option<&str>,
    ) -> Result<()> {
        not_impl("merge")
    }

    async fn merge_abort(&self, _repo: &RepoId) -> Result<()> {
        not_impl("merge_abort")
    }

    async fn merge_continue(&self, _repo: &RepoId) -> Result<()> {
        not_impl("merge_continue")
    }

    async fn cherry_pick(&self, _repo: &RepoId, _commit: &str) -> Result<()> {
        not_impl("cherry_pick")
    }

    async fn cherry_pick_abort(&self, _repo: &RepoId) -> Result<()> {
        not_impl("cherry_pick_abort")
    }

    async fn cherry_pick_continue(&self, _repo: &RepoId) -> Result<()> {
        not_impl("cherry_pick_continue")
    }

    /// revert 冲突后中止（`git revert --abort`）
    async fn revert_abort(&self, _repo: &RepoId) -> Result<()> {
        not_impl("revert_abort")
    }

    /// revert 冲突解决后继续（`git revert --continue`）
    async fn revert_continue(&self, _repo: &RepoId) -> Result<()> {
        not_impl("revert_continue")
    }

    /// 采纳 HEAD 侧（`git checkout --ours` + `git add`）
    async fn use_ours(&self, _repo: &RepoId, _paths: &[String]) -> Result<()> {
        not_impl("use_ours")
    }

    /// 采纳对方分支侧
    async fn use_theirs(&self, _repo: &RepoId, _paths: &[String]) -> Result<()> {
        not_impl("use_theirs")
    }

    /// 重置 HEAD。Hard 会丢未提交改动，UI 须弹二次确认
    async fn reset(&self, _repo: &RepoId, _target: &str, _kind: ResetKind) -> Result<()> {
        not_impl("reset")
    }

    /// 生成反向 commit 撤销指定 commit，不改写历史
    async fn revert(&self, _repo: &RepoId, _commit: &str) -> Result<()> {
        not_impl("revert")
    }

    /// 把当前分支 rebase 到 onto
    async fn rebase(&self, _repo: &RepoId, _onto: &str) -> Result<()> {
        not_impl("rebase")
    }

    async fn rebase_continue(&self, _repo: &RepoId) -> Result<()> {
        not_impl("rebase_continue")
    }

    /// 丢弃当前 commit 继续下一个
    async fn rebase_skip(&self, _repo: &RepoId) -> Result<()> {
        not_impl("rebase_skip")
    }

    async fn rebase_abort(&self, _repo: &RepoId) -> Result<()> {
        not_impl("rebase_abort")
    }

    async fn list_remotes(&self, _repo: &RepoId) -> Result<Vec<Remote>> {
        not_impl("list_remotes")
    }

    async fn add_remote(&self, _repo: &RepoId, _name: &str, _url: &str) -> Result<()> {
        not_impl("add_remote")
    }

    async fn remove_remote(&self, _repo: &RepoId, _name: &str) -> Result<()> {
        not_impl("remove_remote")
    }

    /// 修改 fetch URL（push URL 跟随 fetch）
    async fn set_remote_url(&self, _repo: &RepoId, _name: &str, _url: &str) -> Result<()> {
        not_impl("set_remote_url")
    }

    /// 重命名远程（`git remote rename`），保留其 URL 与分支跟踪配置
    async fn rename_remote(&self, _repo: &RepoId, _old: &str, _new: &str) -> Result<()> {
        not_impl("rename_remote")
    }

    /// commit 引入的文件变更。`staged` 承载该 commit 的变更类型，`unstaged` 始终 None
    async fn list_commit_files(&self, _repo: &RepoId, _commit: &str) -> Result<Vec<FileStatus>> {
        not_impl("list_commit_files")
    }

    /// 按当前行号 1-based 顺序，长度等于文件总行数
    async fn blame(&self, _repo: &RepoId, _path: &str) -> Result<Vec<BlameLine>> {
        not_impl("blame")
    }

    /// ref_name=None 等价 HEAD；limit=None 用 git 默认
    async fn list_reflog(
        &self,
        _repo: &RepoId,
        _ref_name: Option<&str>,
        _limit: Option<usize>,
    ) -> Result<Vec<ReflogEntry>> {
        not_impl("list_reflog")
    }

    /// Clone 远程仓库，dest 必须不存在或空目录
    async fn clone_repo(&self, _url: &str, _dest: &Path) -> Result<RepoConfig> {
        not_impl("clone_repo")
    }

    /// Clone（带进度 + 可取消）：实现方把人类可读进度行持续写入 `progress` 共享槽
    /// （UI 每帧读取展示），`cancel` 置位后中止并返回错误（与 CancelHandle 同哲学，
    /// 保持 dyn-safe）。默认回退到无进度 clone_repo
    async fn clone_repo_streaming(
        &self,
        url: &str,
        dest: &Path,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> Result<RepoConfig> {
        let _ = (cancel, progress);
        self.clone_repo(url, dest).await
    }

    /// `git init`
    async fn init_repo(&self, _path: &Path) -> Result<RepoConfig> {
        not_impl("init_repo")
    }

    /// 获取 interactive rebase 初始计划，全部 Pick
    async fn interactive_rebase_plan(
        &self,
        _repo: &RepoId,
        _onto: &str,
    ) -> Result<Vec<RebaseTodo>> {
        not_impl("interactive_rebase_plan")
    }

    /// 写 todo 文件后调 `git rebase -i`
    async fn interactive_rebase_execute(
        &self,
        _repo: &RepoId,
        _onto: &str,
        _todos: &[RebaseTodo],
    ) -> Result<()> {
        not_impl("interactive_rebase_execute")
    }

    /// 取冲突文件三方内容（ours=stage2、theirs=stage3、base=stage1）
    async fn get_conflict_content(&self, _repo: &RepoId, _path: &str) -> Result<ConflictContent> {
        not_impl("get_conflict_content")
    }
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
