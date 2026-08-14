#[derive(Clone, Copy)]
pub struct ShortcutSpec {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub action: Option<&'static str>,
    pub context: Option<&'static str>,
    pub default_key: &'static str,
    pub macos: &'static str,
    pub windows: &'static str,
    pub linux: &'static str,
}

macro_rules! shortcut {
    ($id:literal, $group:literal, $label:literal, $action:literal, $context:expr, $key:literal, $mac:literal, $other:literal) => {
        ShortcutSpec {
            id: $id,
            group: $group,
            label: $label,
            action: Some($action),
            context: $context,
            default_key: $key,
            macos: $mac,
            windows: $other,
            linux: $other,
        }
    };
}

pub(super) const SHORTCUTS: &[ShortcutSpec] = &[
    shortcut!(
        "open-recent",
        "全局",
        "选择连接",
        "ramag::OpenRecentItems",
        None,
        "secondary-p",
        "⌘P",
        "Ctrl+P"
    ),
    ShortcutSpec {
        id: "wake-main-window",
        group: "全局",
        label: "唤醒 Ramag",
        action: None,
        context: None,
        default_key: "secondary-alt-shift-v",
        macos: "⌘⌥⇧V",
        windows: "Ctrl+Alt+Shift+V",
        linux: "暂不支持",
    },
    shortcut!(
        "close-tab",
        "全局",
        "关闭标签",
        "ramag::CloseTab",
        None,
        "secondary-w",
        "⌘W",
        "Ctrl+W"
    ),
    shortcut!(
        "quit",
        "全局",
        "退出应用",
        "ramag::Quit",
        None,
        "secondary-q",
        "⌘Q",
        "Ctrl+Q"
    ),
    shortcut!(
        "run-query",
        "数据库",
        "执行查询",
        "ramag_dbclient::RunQuery",
        None,
        "secondary-enter",
        "⌘Enter",
        "Ctrl+Enter"
    ),
    shortcut!(
        "run-statement",
        "数据库",
        "执行当前语句",
        "ramag_dbclient::RunStatementAtCursor",
        None,
        "secondary-shift-enter",
        "⌘⇧Enter",
        "Ctrl+Shift+Enter"
    ),
    shortcut!(
        "new-query",
        "数据库",
        "新建查询",
        "ramag_dbclient::NewQueryTab",
        None,
        "secondary-t",
        "⌘T",
        "Ctrl+T"
    ),
    shortcut!(
        "find-results",
        "数据库",
        "结果筛选",
        "ramag_dbclient::FindInResults",
        None,
        "secondary-f",
        "⌘F",
        "Ctrl+F"
    ),
    shortcut!(
        "format-sql",
        "数据库",
        "格式化 SQL",
        "ramag_dbclient::FormatSql",
        None,
        "secondary-shift-f",
        "⌘⇧F",
        "Ctrl+Shift+F"
    ),
    shortcut!(
        "toggle-editor",
        "数据库",
        "切换编辑器",
        "ramag_dbclient::ToggleSqlEditor",
        None,
        "secondary-e",
        "⌘E",
        "Ctrl+E"
    ),
    shortcut!(
        "toggle-redis-console",
        "数据库",
        "切换 Redis 控制台",
        "ramag_redis::ToggleRedisConsole",
        Some("RedisSession"),
        "secondary-e",
        "⌘E",
        "Ctrl+E"
    ),
    shortcut!(
        "vcs-commit",
        "Git",
        "提交",
        "ramag_vcs::CommitNow",
        Some("VcsView"),
        "secondary-enter",
        "⌘Enter",
        "Ctrl+Enter"
    ),
    shortcut!(
        "vcs-push",
        "Git",
        "推送",
        "ramag_vcs::PushNow",
        Some("VcsView"),
        "secondary-shift-k",
        "⌘⇧K",
        "Ctrl+Shift+K"
    ),
    shortcut!(
        "vcs-pull",
        "Git",
        "拉取",
        "ramag_vcs::PullNow",
        Some("VcsView"),
        "secondary-t",
        "⌘T",
        "Ctrl+T"
    ),
    shortcut!(
        "ssh-new-terminal",
        "SSH",
        "新建终端",
        "ssh::NewSshTerminal",
        Some("SshWorkspace"),
        "secondary-t",
        "⌘T",
        "Ctrl+T"
    ),
    shortcut!(
        "ssh-close-terminal",
        "SSH",
        "关闭终端",
        "ssh::CloseSshTerminal",
        Some("SshWorkspace"),
        "secondary-w",
        "⌘W",
        "Ctrl+W"
    ),
];

pub(super) const MODULE_GROUPS: &[&str] = &["数据库", "Git", "SSH", "对象存储", "剪贴板"];
