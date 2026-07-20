//! VCS 视图共享类型 + 辅助函数：视图状态枚举 / 行尾按钮 / 文件状态色 / commit 行渲染

mod commit_row;

pub(super) use commit_row::render_commit_row;

use gpui::{AnyElement, ClickEvent, Context, IntoElement, SharedString, Window};
use gpui_component::{Disableable as _, IconName, Sizable as _, button::ButtonVariants as _};
use ramag_domain::entities::{Branch, FileChangeKind, FileDiff, Remote};

use super::vcs_view::VcsView;

/// 异步状态槽必须按 Arc 身份确认归属，不能只判断槽非空；否则旧任务会误伤后续任务。
pub(super) fn is_current_arc_slot<T>(
    current: Option<&std::sync::Arc<T>>,
    expected: &std::sync::Arc<T>,
) -> bool {
    current.is_some_and(|current| std::sync::Arc::ptr_eq(current, expected))
}

/// 主视图当前展示模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewMode {
    /// 工作区（变更 / commit / 分支）
    Workspace,
    /// 历史日志
    History,
}

/// VcsView 顶层视图：RepoList（仓库管理）/ Session（IDE 布局）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActiveView {
    RepoList,
    Session,
}

/// Files panel 视图：Project（默认，完整目录树）/ Changes（变更分组）/ Stash / Branches
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FilesViewMode {
    Project,
    Changes,
    Stash,
}

impl FilesViewMode {
    /// 用于 tooltip 的中文标签
    pub(super) fn label(self) -> &'static str {
        match self {
            FilesViewMode::Project => "项目文件",
            FilesViewMode::Changes => "本地变更",
            FilesViewMode::Stash => "暂存堆栈",
        }
    }

    /// 用于 tab 按钮的 dom id 后缀
    pub(super) fn id_str(self) -> &'static str {
        match self {
            FilesViewMode::Project => "project",
            FilesViewMode::Changes => "changes",
            FilesViewMode::Stash => "stash",
        }
    }
}

/// History 面板每页加载条数
pub(super) const HISTORY_PAGE_SIZE: usize = 100;

/// Diff 视图二态：[`Standard`]=带少量上下文（git -U3，默认）/ [`FullFile`]=展示全文件（-U999999）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffViewMode {
    Standard,
    FullFile,
}

impl DiffViewMode {
    /// 后端 unified 上下文行数：3=标准；999999=全文件
    pub(super) fn context_lines(self) -> u32 {
        match self {
            DiffViewMode::Standard => 3,
            DiffViewMode::FullFile => 999_999,
        }
    }

    /// 切换：标准 ↔ 全文件
    pub(super) fn toggled(self) -> Self {
        match self {
            DiffViewMode::Standard => DiffViewMode::FullFile,
            DiffViewMode::FullFile => DiffViewMode::Standard,
        }
    }
}

/// 文件级写操作种类（行尾按钮触发）
#[derive(Debug, Clone, Copy)]
pub(super) enum FileOp {
    Stage,
    Unstage,
    Discard,
}

/// 远程同步操作种类（顶部工具栏按钮触发）
#[derive(Debug, Clone, Copy)]
pub(super) enum RemoteOp {
    Fetch,
    Pull,
    /// 普通 push（force=false）
    Push,
    /// 安全强推（git push --force-with-lease）—— 用于改写历史后推送（rebase / amend）
    PushForce,
}

/// 文件分组所属（决定行尾按钮的 stage/unstage/discard 组合）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupKind {
    Staged,
    Unstaged,
    Untracked,
    Conflict,
}

/// Project Files 文件内容快照。正文完整保留，编辑器负责增量解析和可见行渲染。
#[derive(Clone)]
pub(super) struct FileContentSnapshot {
    /// 仓库根的相对路径（与 ls-files 输出一致）
    pub path: String,
    /// 当前正文；未保存编辑也保存在对应文件标签中，切换标签不会丢失。
    pub text: std::rc::Rc<String>,
    /// 当前正文行数，避免渲染标题时重复扫描全文。
    pub line_count: usize,
    /// 编辑代际；异步保存只可清理同一代草稿的 dirty 状态。
    pub revision: u64,
    /// 是否含尚未写回磁盘的用户修改。
    pub dirty: bool,
    /// 是否被 4MB 阈值截断；截断预览禁止编辑，避免保存时破坏未加载的尾部。
    pub truncated: bool,
    /// 是否被识别为二进制（前 8KB 含 NUL 字节即视为二进制）
    pub binary: bool,
    /// 读盘失败时的错误描述（None = 成功）
    pub error: Option<String>,
}

/// Render 持有 Window 时写入 Code Editor；路径校验防止旧的 defer 覆盖新标签。
pub(super) struct PendingFileEditorLoad {
    pub path: String,
    pub text: std::rc::Rc<String>,
    pub language: gpui::SharedString,
}

/// 文件 tab 来源：Changes（工作区 diff）/ ProjectFiles（内容）/ Commit（commit diff），共用一套主区渲染
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FileTabSource {
    Changes(GroupKind),
    ProjectFiles,
    /// commit_id：完整 hash；change_kind：Modified/Added/Deleted/Renamed/...（决定状态字母）
    Commit {
        commit_id: String,
        change_kind: Option<FileChangeKind>,
    },
}

/// 主区已打开的文件 tab（统一服务 Changes diff 和 ProjectFiles 内容）
#[derive(Clone)]
pub(super) struct FileTab {
    pub path: String,
    pub source: FileTabSource,
    /// Changes 来源拉到的 diff（ProjectFiles 始终 None）
    pub cached_diff: Option<std::rc::Rc<FileDiff>>,
    /// 与 cached_diff 同代的持久语法树；内容超限或不支持语言时仍保留纯文本快照。
    pub cached_diff_syntax: Option<std::rc::Rc<super::syntax::DiffSyntaxSnapshot>>,
    /// ProjectFiles 来源读到的文件内容快照（Changes 始终 None）
    pub cached_content: Option<FileContentSnapshot>,
}

/// Stash 行尾按钮触发的操作
#[derive(Debug, Clone, Copy)]
pub(super) enum StashOp {
    /// 应用某个 stash（不删）
    Apply(usize),
    /// 应用某个 stash 后删除
    Pop(usize),
    /// 仅删除某个 stash
    Drop(usize),
}

/// 分支操作（checkout / create / delete / merge / rebase）
#[derive(Debug, Clone)]
pub(super) enum BranchOp {
    Checkout(String),
    /// (name, base) — base=None 从 HEAD 创建；创建后会自动 checkout 到新分支
    Create(String, Option<String>),
    /// (name, force) — force=true 用 -D 强制删未合并分支
    Delete(String, bool),
    /// 把指定分支合并到当前 HEAD（默认 --no-ff，强制建 merge commit）
    Merge(String),
    /// 把当前 HEAD rebase 到指定分支上（git rebase &lt;name&gt;）
    Rebase(String),
}

/// 远程分支必须落到本地 tracking 分支，不能直接 checkout 成 detached HEAD。
pub(super) fn checkout_remote_branch_op(
    remote_branch: &str,
    local_branches: &[Branch],
) -> Result<BranchOp, String> {
    let Some((_, local_name)) = remote_branch.split_once('/') else {
        return Err(format!("远程分支名无效：{remote_branch}"));
    };
    if local_name.is_empty() {
        return Err(format!("远程分支名无效：{remote_branch}"));
    }
    match local_branches
        .iter()
        .find(|branch| branch.name == local_name)
    {
        None => Ok(BranchOp::Create(
            local_name.to_string(),
            Some(remote_branch.to_string()),
        )),
        Some(branch) if branch.upstream.as_deref() == Some(remote_branch) => {
            Ok(BranchOp::Checkout(local_name.to_string()))
        }
        Some(branch) => Err(format!(
            "本地分支「{local_name}」已存在，但上游是「{}」，未自动改写关联；请切换该本地分支或先在 Git 中调整 upstream",
            branch.upstream.as_deref().unwrap_or("未设置")
        )),
    }
}

/// 首次 Push / Tag Push 的默认 remote：约定优先 origin，否则仅在唯一候选时自动选择。
pub(super) fn default_remote_name(remotes: &[Remote]) -> Result<String, String> {
    if remotes.iter().any(|remote| remote.name == "origin") {
        return Ok("origin".into());
    }
    match remotes {
        [remote] => Ok(remote.name.clone()),
        [] => Err("当前仓库没有 remote：请先配置远程仓库再 Push".into()),
        _ => Err(format!(
            "当前仓库有多个 remote（{}），且没有 origin；请先为分支设置 upstream，避免推送到错误仓库",
            remotes
                .iter()
                .map(|remote| remote.name.as_str())
                .collect::<Vec<_>>()
                .join("、")
        )),
    }
}

/// 首次 Push 在“多个 remote 且没有 origin”时必须让用户显式选择，不能猜目标。
pub(super) fn needs_first_push_remote_picker(
    op: RemoteOp,
    remotes: &[Remote],
    upstream: Option<&str>,
) -> bool {
    matches!(op, RemoteOp::Push | RemoteOp::PushForce)
        && upstream.is_none()
        && remotes.len() > 1
        && !remotes.iter().any(|remote| remote.name == "origin")
}

/// 冲突文件解决操作（行尾按钮触发）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConflictOp {
    /// 采纳「我们」（HEAD 侧）的版本
    UseOurs,
    /// 采纳「他们」（对方分支）的版本
    UseTheirs,
    /// 单纯标记为已解决（用户手改后调）= git add
    MarkResolved,
}

/// 进行中操作的「继续 / 中止 / 跳过」按钮触发
///
/// `Skip` 仅 rebase 支持（merge / cherry-pick 时按钮置灰）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationStep {
    Continue,
    Abort,
    Skip,
}

/// 进行中操作与步骤的用户可见名称（错误横幅用，避免暴露 Debug 枚举名）
pub(super) fn operation_label(op: ramag_domain::entities::RepoOperation) -> &'static str {
    use ramag_domain::entities::RepoOperation;
    match op {
        RepoOperation::Merge => "合并",
        RepoOperation::Rebase => "Rebase",
        RepoOperation::CherryPick => "Cherry-pick",
        RepoOperation::Revert => "Revert",
    }
}

pub(super) fn step_label(step: OperationStep) -> &'static str {
    match step {
        OperationStep::Continue => "继续",
        OperationStep::Abort => "中止",
        OperationStep::Skip => "跳过",
    }
}

/// Reset 模式的用户可见名（与右键菜单里的 --soft/--mixed/--hard 写法一致）
pub(super) fn reset_kind_label(kind: ramag_domain::entities::ResetKind) -> &'static str {
    use ramag_domain::entities::ResetKind;
    match kind {
        ResetKind::Soft => "--soft",
        ResetKind::Mixed => "--mixed",
        ResetKind::Hard => "--hard",
    }
}

/// Tag 操作（创建 / 删除 / 推送）
#[derive(Debug, Clone)]
pub(super) enum TagOp {
    /// (name, message=None 表示 lightweight；Some 创建 annotated；target=None 基于 HEAD)
    Create {
        name: String,
        message: Option<String>,
    },
    /// 删除本地 tag
    Delete(String),
    /// 推送 tag 到 origin
    Push(String),
}

/// 行尾操作小按钮：触发 self.run_file_op（已转图标按钮 + tooltip）
pub(super) fn file_op_button(
    id_parts: (&'static str, usize),
    label: &'static str,
    op: FileOp,
    path: String,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let id = SharedString::from(format!("vcs-{}-{}", id_parts.0, id_parts.1));
    let mut btn = ramag_ui::clickable_button(id)
        .ghost()
        .xsmall()
        .tooltip(label)
        .disabled(busy);
    btn = match op {
        FileOp::Stage => btn.icon(IconName::Plus),
        FileOp::Unstage => btn.icon(IconName::Minus),
        FileOp::Discard => btn.icon(ramag_ui::icons::trash()),
    };
    btn.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
        this.confirm_file_op(op, path.clone(), window, cx);
    }))
    .into_any_element()
}

/// 侧栏行尾操作小按钮（stash / tag 等通用）：ghost + xsmall + icon + tooltip + disabled。
/// `id` 由调用方拼好（含前缀去重），`on_click` 由调用方按各自操作构造。
pub(super) fn side_op_button(
    id: impl Into<SharedString>,
    tooltip: &'static str,
    icon: impl Into<gpui_component::Icon>,
    busy: bool,
    on_click: impl Fn(&mut VcsView, &mut Window, &mut Context<VcsView>) + 'static,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    ramag_ui::clickable_button(id.into())
        .ghost()
        .xsmall()
        .icon(icon)
        .tooltip(tooltip)
        .disabled(busy)
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            on_click(this, window, cx);
        }))
        .into_any_element()
}

/// 文件状态字母（M / A / D / R / C / T / ? / U）
pub(super) fn code_to_letter(kind: Option<FileChangeKind>) -> &'static str {
    match kind {
        Some(FileChangeKind::Modified) => "M",
        Some(FileChangeKind::Added) => "A",
        Some(FileChangeKind::Deleted) => "D",
        Some(FileChangeKind::Renamed) => "R",
        Some(FileChangeKind::Copied) => "C",
        Some(FileChangeKind::TypeChanged) => "T",
        Some(FileChangeKind::Untracked) => "?",
        Some(FileChangeKind::Conflicted) => "U",
        None => " ",
    }
}

/// 不同变更类型用不同颜色（M 暖橙 / A 绿 / D 红 / R 蓝 / U 深红）
pub(super) fn code_letter_color(code: &str, fallback: gpui::Hsla) -> gpui::Hsla {
    match code {
        "M" => gpui::hsla(40.0 / 360.0, 0.7, 0.55, 1.0),
        "A" => gpui::hsla(140.0 / 360.0, 0.55, 0.45, 1.0),
        "D" => gpui::hsla(0.0, 0.65, 0.55, 1.0),
        "R" => gpui::hsla(220.0 / 360.0, 0.6, 0.55, 1.0),
        "C" => gpui::hsla(220.0 / 360.0, 0.6, 0.55, 1.0),
        "T" => gpui::hsla(280.0 / 360.0, 0.55, 0.55, 1.0),
        "U" => gpui::hsla(0.0, 0.75, 0.5, 1.0),
        _ => fallback,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use ramag_domain::entities::{Branch, BranchKind, CommitId, Remote};

    use super::{
        BranchOp, RemoteOp, checkout_remote_branch_op, default_remote_name, is_current_arc_slot,
        needs_first_push_remote_picker,
    };

    fn local(name: &str, upstream: Option<&str>) -> Branch {
        Branch {
            name: name.into(),
            kind: BranchKind::Local,
            commit: CommitId("abc".into()),
            is_head: false,
            upstream: upstream.map(str::to_owned),
            ahead: None,
            behind: None,
        }
    }

    fn remote(name: &str) -> Remote {
        Remote {
            name: name.into(),
            fetch_url: String::new(),
            push_url: None,
        }
    }

    #[test]
    fn remote_branch_creates_or_reuses_matching_tracking_branch() {
        assert!(matches!(
            checkout_remote_branch_op("origin/feature/a", &[]),
            Ok(BranchOp::Create(name, Some(base)))
                if name == "feature/a" && base == "origin/feature/a"
        ));
        assert!(matches!(
            checkout_remote_branch_op(
                "origin/main",
                &[local("main", Some("origin/main"))]
            ),
            Ok(BranchOp::Checkout(name)) if name == "main"
        ));
    }

    #[test]
    fn remote_branch_does_not_retarget_existing_local_branch() {
        let result =
            checkout_remote_branch_op("upstream/main", &[local("main", Some("origin/main"))]);
        assert!(result.is_err());
    }

    #[test]
    fn default_remote_prefers_origin_or_the_only_remote() {
        assert_eq!(
            default_remote_name(&[remote("upstream")]).unwrap(),
            "upstream"
        );
        assert_eq!(
            default_remote_name(&[remote("upstream"), remote("origin")]).unwrap(),
            "origin"
        );
        assert!(default_remote_name(&[remote("a"), remote("b")]).is_err());
    }

    #[test]
    fn first_push_with_ambiguous_remotes_requires_picker() {
        let remotes = [remote("upstream"), remote("fork")];
        assert!(needs_first_push_remote_picker(
            RemoteOp::Push,
            &remotes,
            None
        ));
        assert!(!needs_first_push_remote_picker(
            RemoteOp::Pull,
            &remotes,
            None
        ));
        assert!(!needs_first_push_remote_picker(
            RemoteOp::Push,
            &remotes,
            Some("fork/main")
        ));
        assert!(!needs_first_push_remote_picker(
            RemoteOp::Push,
            &[remote("origin"), remote("upstream")],
            None
        ));
    }

    #[test]
    fn async_slot_identity_rejects_replacement_value() {
        let original = std::sync::Arc::new(false);
        let replacement = std::sync::Arc::new(false);

        assert!(is_current_arc_slot(Some(&original), &original));
        assert!(!is_current_arc_slot(Some(&replacement), &original));
        assert!(!is_current_arc_slot(None, &original));
    }
}
