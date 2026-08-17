use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_COMMIT_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MAX_GIT_NAME_ARG_BYTES: usize = 1024;
pub const MAX_GIT_POSITIONAL_ARG_BYTES: usize = 4 * 1024;
pub const MAX_GIT_PATH_BYTES: usize = 64 * 1024;
pub const MAX_GIT_PATH_DEPTH: usize = 256;
pub const MAX_GIT_PATH_ARGS: usize = 50_000;
pub const MAX_GIT_PATH_ARGS_BYTES: usize = 16 * 1024 * 1024;
/// 文件监听增量 status 走 argv；限制批次以兼容 Windows 命令行长度并控制合并成本。
pub const MAX_INCREMENTAL_STATUS_PATHS: usize = 128;
pub const MAX_INCREMENTAL_STATUS_PATH_BYTES: usize = 16 * 1024;
/// 行级暂存 / 回滚 patch 通过 stdin 传递，限制克隆与子进程写入的峰值内存。
pub const MAX_GIT_PATCH_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_GIT_TAG_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_GIT_STASH_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoId(pub Uuid);

impl RepoId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RepoId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub id: RepoId,
    pub name: String,
    pub path: String,
    pub last_opened_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl RepoConfig {
    pub fn from_path(path: impl Into<String>) -> Self {
        let path: String = path.into();
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        Self {
            id: RepoId::new(),
            name,
            path,
            last_opened_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStatus {
    pub path: String,
    pub old_path: Option<String>,
    pub staged: Option<FileChangeKind>,
    pub unstaged: Option<FileChangeKind>,
}

impl FileStatus {
    pub fn is_conflicted(&self) -> bool {
        matches!(self.staged, Some(FileChangeKind::Conflicted))
            || matches!(self.unstaged, Some(FileChangeKind::Conflicted))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkingTreeStatus {
    /// detached HEAD 时为 `None`。
    pub head_branch: Option<String>,
    pub head_commit: Option<String>,
    pub operation: Option<RepoOperation>,
    pub files: Vec<FileStatus>,
    pub behind: Option<usize>,
    pub ahead: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepoOperation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

/// `git reset` 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetKind {
    /// 移动 HEAD，暂存区 / 工作区不动
    Soft,
    /// 移动 HEAD + 重置暂存区，工作区不动（默认）
    Mixed,
    /// 重置 HEAD + 暂存区 + 工作区（危险，会丢未提交改动）
    Hard,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommitId(pub String);

impl CommitId {
    pub fn short(&self) -> &str {
        char_prefix(&self.0, 7)
    }
}

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parents: Vec<CommitId>,
    pub author: Signature,
    pub committer: Signature,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub refs: Vec<String>,
}

impl Commit {
    pub fn message_full(&self) -> String {
        if self.body.is_empty() {
            self.subject.clone()
        } else {
            format!("{}\n\n{}", self.subject, self.body)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LogOptions {
    /// 起点，None = HEAD
    pub start: Option<String>,
    pub path_filter: Option<String>,
    pub skip: usize,
    pub limit: Option<usize>,
    pub current_branch_only: bool,
    /// `--grep=`：按 message 关键词过滤
    pub grep: Option<String>,
    /// `--author=`：可填名字或邮箱片段
    pub author: Option<String>,
    /// `--since=`：git 自然时间，如 "1 week ago"
    pub since: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchKind {
    Local,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    /// 短名，不含 `refs/heads/` / `refs/remotes/<remote>/` 前缀
    pub name: String,
    pub kind: BranchKind,
    pub commit: CommitId,
    pub is_head: bool,
    /// 上游分支（如 `origin/main`），仅 Local 有意义
    pub upstream: Option<String>,
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLineKind {
    Context,
    Add,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// 旧文件行号，None = 新增行
    pub old_lineno: Option<u32>,
    /// 新文件行号，None = 删除行
    pub new_lineno: Option<u32>,
    /// 行内容，不含 `+/-/<space>` 前缀
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub heading: Option<String>,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub change_kind: FileChangeKind,
    pub binary: bool,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflogEntry {
    pub commit: CommitId,
    /// 形如 "HEAD@{0}"
    pub selector: String,
    pub action: String,
    pub subject: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameLine {
    pub commit: CommitId,
    pub author: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub line_no: u32,
    pub subject: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RebaseAction {
    Pick,
    Squash,
    Fixup,
    Reword,
    Edit,
    Drop,
}

impl RebaseAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Reword => "reword",
            Self::Edit => "edit",
            Self::Drop => "drop",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Pick => "保留",
            Self::Squash => "合并保留",
            Self::Fixup => "合并丢弃",
            Self::Reword => "改说明",
            Self::Edit => "暂停",
            Self::Drop => "删除",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebaseTodo {
    pub action: RebaseAction,
    pub hash: String,
    pub subject: String,
}

impl RebaseTodo {
    pub fn short_hash(&self) -> &str {
        char_prefix(&self.hash, 7)
    }
}

fn char_prefix(value: &str, max_chars: usize) -> &str {
    value
        .char_indices()
        .nth(max_chars)
        .map_or(value, |(end, _)| &value[..end])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictContent {
    pub path: String,
    /// stage 2：HEAD 侧
    pub ours: Vec<String>,
    /// stage 3：MERGE_HEAD 侧
    pub theirs: Vec<String>,
    /// stage 1：共同祖先
    pub base: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffKind {
    WorkingTreeVsIndex,
    IndexVsHead,
    WorkingTreeVsHead,
    CommitVsParent(CommitId),
    Range { from: CommitId, to: CommitId },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StashId(pub usize);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stash {
    pub id: StashId,
    pub message: String,
    pub commit: CommitId,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub fetch_url: String,
    pub push_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagKind {
    Lightweight,
    Annotated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub kind: TagKind,
    pub commit: CommitId,
    pub message: Option<String>,
    pub tagger: Option<Signature>,
}

#[cfg(test)]
mod tests {
    use super::{CommitId, RebaseAction, RebaseTodo};

    #[test]
    fn short_hash_helpers_preserve_utf8_boundaries() {
        let commit = CommitId("提交编号一二三四五六七八".into());
        let todo = RebaseTodo {
            action: RebaseAction::Pick,
            hash: "哈希一二三四五六七八".into(),
            subject: String::new(),
        };

        assert_eq!(commit.short().chars().count(), 7);
        assert_eq!(todo.short_hash().chars().count(), 7);
    }
}
