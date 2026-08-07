#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod clipboard_runtime;
mod composition;
mod logging;
#[cfg(any(target_os = "windows", target_os = "linux"))]
#[cfg_attr(target_os = "linux", path = "single_instance_linux.rs")]
mod single_instance;
#[cfg(target_os = "windows")]
mod tray;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod window_layout;
mod windows;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use clipboard_runtime::*;
use composition::*;
use windows::*;

use std::collections::HashMap;
use std::sync::Arc;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use gpui::WindowKind;
use gpui::{
    Action, App, Bounds, KeyBinding, Menu, MenuItem, Subscription, TitlebarOptions, WindowBounds,
    WindowOptions, prelude::*, px, size,
};
use gpui_component::Root;
use ramag_app::{
    ClipboardService, ConnectionService, DataSyncGate, DataSyncService, MongoService, RedisService,
    SshService, ToolRegistry, UpdateService,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ramag_domain::traits::ClipboardDriver;
use ramag_domain::traits::{
    DocDriver, Driver, GitDriver, JumpServerDriver, KvDriver, SshDriver, Storage,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ramag_infra_clipboard::{HotkeyListener, PlatformClipboardDriver, foreground_display_index};
use ramag_infra_git::GitDriverImpl;
use ramag_infra_mongodb::MongoDriver;
use ramag_infra_mysql::MysqlDriver;
use ramag_infra_postgres::PostgresDriver;
use ramag_infra_redis::RedisDriver;
use ramag_infra_ssh::{JumpServerHttpDriver, OpenSshDriver};
use ramag_infra_storage::RedbStorage;
use ramag_infra_update::GitHubUpdateDriver;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ramag_tool_clipboard::{
    ClipboardTool, CopySelectedClip, DeleteSelectedClip, FocusClipSearch, SelectNextClip,
    SelectPrevClip, create_clipboard_drawer, create_clipboard_view,
};
use ramag_tool_dbclient::{
    DbClientTool, ExplainQuery, FindInResults, FormatSql, NewQueryTab, RunQuery,
    RunStatementAtCursor, ToggleRedisConsole, ToggleSqlEditor, create_dbclient_view,
};
use ramag_tool_mongodb::{FormatMongoJson, NewMongoQueryTab, RunMongoQuery, ToggleMongoEditor};
use ramag_tool_ssh::{CloseSshTerminal, NewSshTerminal, RefreshSftp, SshTool, create_ssh_view};
use ramag_tool_vcs::{
    CommitNow, FocusCommitMessage, PullNow, PushNow, RefreshWorkspace, SaveProjectFile,
    ToggleHistoryPane, VcsTool, create_vcs_view,
};
use ramag_ui::{
    CloseTab, CycleSection, CycleSectionReverse, DATABASE_SEARCH_SETTINGS_PREF_KEY,
    FEEDBACK_ISSUE_URL, HomeEvent, HomeView, NavTarget, RamagAssets, SelectTool1, SelectTool2,
    SelectTool3, SelectTool4, SettingsView, Shell, StorageGlobal, init_database_search_settings,
    init_theme, sync_update_indicator,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{error, info, warn};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::window_layout::{drawer_bounds, preferred_display};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct Quit;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct OpenLogDir;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct OpenFeedbackIssue;

fn open_path_in_file_manager(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(dir);
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "系统文件管理器退出状态：{status}"
        )))
    }
}

/// 主窗口重建时复用的依赖。
#[derive(Clone)]
struct AppDeps {
    registry: Arc<ToolRegistry>,
    conn_service: Arc<ConnectionService>,
    redis_service: Arc<RedisService>,
    mongo_service: Arc<MongoService>,
    data_sync_service: Arc<DataSyncService>,
    data_sync_gate: Arc<DataSyncGate>,
    clipboard_service: Option<Arc<ClipboardService>>,
    ssh_service: Arc<SshService>,
    update_service: Option<Arc<UpdateService>>,
    storage: Arc<dyn Storage>,
}

/// 托盘和单实例激活优先复用此窗口。
struct MainWindowGlobal(gpui::AnyWindowHandle);

impl gpui::Global for MainWindowGlobal {}

/// `open_window` 在异步任务中完成；句柄写入全局前，重复唤起必须被合并。
#[derive(Default)]
struct MainWindowOpenGate {
    opening: bool,
}

impl gpui::Global for MainWindowOpenGate {}

impl MainWindowOpenGate {
    fn try_begin(&mut self) -> bool {
        if self.opening {
            return false;
        }
        self.opening = true;
        true
    }

    fn finish(&mut self) {
        self.opening = false;
    }
}

fn begin_main_window_open(cx: &mut App) -> bool {
    if !cx.has_global::<MainWindowOpenGate>() {
        cx.set_global(MainWindowOpenGate::default());
    }
    cx.global_mut::<MainWindowOpenGate>().try_begin()
}

fn finish_main_window_open(cx: &mut App) {
    if cx.has_global::<MainWindowOpenGate>() {
        cx.global_mut::<MainWindowOpenGate>().finish();
    }
}

fn reveal_main_window(deps: &AppDeps, cx: &mut App) {
    if let Some(handle) = cx.try_global::<MainWindowGlobal>().map(|g| g.0)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    open_main_window(deps.clone(), cx);
}

fn main() {
    if let Some(exit_code) = ramag_infra_ssh::run_askpass_helper(confirm_ssh_host) {
        std::process::exit(exit_code);
    }

    let log_path = logging::init();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        process_id = std::process::id(),
        debug = cfg!(debug_assertions),
        "application starting"
    );

    // 单实例：已有实例在跑则通知其唤起主窗口后静默退出（避免 redb 文件锁报错）；
    // macOS 由系统 LaunchServices 保证 .app 单实例，无需自建
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let instance_guard = match single_instance::acquire() {
        single_instance::InstanceRole::Secondary => {
            info!("another instance is running; asked it to reveal and exiting");
            return;
        }
        single_instance::InstanceRole::Primary(guard) => guard,
    };

    let (conn_service, storage) = match build_connection_service() {
        Ok(pair) => pair,
        Err(e) => {
            error!(error = %e, "data layer initialization failed");
            let log_hint = log_path.as_ref().map_or_else(
                || "\n\n日志文件也无法创建，请检查用户目录权限。".to_string(),
                |path| format!("\n\n日志：{}", path.display()),
            );
            let _ = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Ramag 启动失败")
                .set_description(format!("无法初始化本地数据或系统凭据库：\n\n{e}{log_hint}"))
                .show();
            std::process::exit(1);
        }
    };

    let redis_service: Arc<RedisService> = build_redis_service(storage.clone());
    let mongo_service: Arc<MongoService> = build_mongo_service(storage.clone());
    let data_sync_gate = Arc::new(DataSyncGate::default());
    let data_sync_service = Arc::new(DataSyncService::new(
        conn_service.clone(),
        mongo_service.clone(),
        data_sync_gate.clone(),
    ));
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let clipboard_service = Some(build_clipboard_service(storage.clone()));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let clipboard_service: Option<Arc<ClipboardService>> = None;
    let ssh_service: Arc<SshService> = build_ssh_service(storage.clone());
    let update_service = build_update_service(storage.clone());

    // 主题偏好。"dark" 用暗色，其余（含旧版 "system" 残值）默认浅色
    let startup_preferences =
        read_preferences(&storage, &["theme_mode", DATABASE_SEARCH_SETTINGS_PREF_KEY]);
    let initial_pref = startup_preferences.get("theme_mode").cloned();
    let initial_database_search_pref = startup_preferences
        .get(DATABASE_SEARCH_SETTINGS_PREF_KEY)
        .cloned();

    // 剪贴板总开关决定工具入口可见性；启动同步读取，避免「恢复上次工具」误入已隐藏的剪贴板
    let registry = build_tool_registry();
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let clipboard_enabled = clipboard_service.as_ref().is_some_and(|service| {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(service.prime_capture_enabled()),
                Err(error) => {
                    warn!(error = %error, fallback = "tool_hidden", "load clipboard settings failed");
                    false
                }
            }
        });
        registry.set_enabled(ClipboardTool::ID, clipboard_enabled);
    }
    info!(tool_count = registry.count(), "tools registered");

    let deps = AppDeps {
        registry,
        conn_service,
        redis_service,
        mongo_service,
        data_sync_service,
        data_sync_gate,
        clipboard_service,
        ssh_service,
        update_service,
        storage,
    };

    let app = gpui_platform::application().with_assets(RamagAssets);

    // on_reopen 必须在 app.run 之前注册（属 Application）。仅当无活窗口时重开主窗口，避免 dock 叠加
    let deps_for_reopen = deps.clone();
    app.on_reopen(move |cx: &mut App| {
        if cx.windows().is_empty() {
            reveal_main_window(&deps_for_reopen, cx);
        }
    });

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        ramag_tool_ssh::init(cx);
        init_theme(initial_pref.as_deref(), cx);
        if let Err(error) =
            init_database_search_settings(initial_database_search_pref.as_deref(), cx)
        {
            warn!(error, "ignore invalid database search settings");
        }
        cx.set_global(StorageGlobal(deps.storage.clone()));
        cx.activate(true);

        let data_sync_gate_for_quit = deps.data_sync_gate.clone();
        cx.on_action(move |_: &Quit, cx| {
            if !data_sync_gate_for_quit.is_blocking() {
                cx.quit();
            }
        });

        // 退出时关闭全部 SSH 隧道子进程，避免残留孤儿 ssh 占用端口
        let ssh_service_for_quit = deps.ssh_service.clone();
        cx.on_app_quit(move |_| {
            let ssh_service = ssh_service_for_quit.clone();
            async move {
                if let Err(error) = ssh_service.shutdown().await {
                    warn!(error = %error, "shutdown ssh tool resources failed");
                }
                ramag_infra_tunnel::shutdown_all();
            }
        })
        .detach();

        // Windows：托盘常驻——关窗后采集继续，托盘可唤回/退出；
        // 托盘安装失败则回退「关最后窗口即退出」，避免无处唤回的无形后台进程。
        // macOS 保留「关窗不退出」+ dock on_reopen，无需此回调
        #[cfg(target_os = "windows")]
        {
            let tray = std::rc::Rc::new(std::cell::RefCell::new(tray::TrayIcon::install()));
            let tray_resident = tray.borrow().is_some();
            if tray_resident {
                spawn_tray_loop(tray.clone(), deps.clone(), cx);
            } else {
                warn!("tray unavailable; app quits when the last window closes");
            }
            // 任何退出路径（cmd-Q / 托盘退出）先删托盘图标，避免任务栏残影
            cx.on_app_quit(move |_| {
                let tray = tray.borrow_mut().take();
                async move {
                    drop(tray);
                }
            })
            .detach();
            cx.on_window_closed(move |cx, _window_id| {
                if !tray_resident && cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            // 双开的后启实例经命名事件请求唤起
            spawn_instance_activation(instance_guard, deps.clone(), cx);
        }

        // Linux 无后台常驻功能：关闭最后一个窗口即退出；重复启动会唤起已有实例。
        #[cfg(target_os = "linux")]
        {
            cx.on_window_closed(|cx, _window_id| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            spawn_instance_activation(instance_guard, deps.clone(), cx);
        }

        // 主修饰键+W 全局 fallback：视图层先消费（关 tab），没消费就关窗。
        // 关窗须 defer：此刻正处在该窗口的按键分发栈内（window 已被 take 出），
        // 直接 handle.update 会重入 take 失败而静默不关；defer 到本次分发结束后再移除
        cx.on_action(|_: &CloseTab, cx: &mut App| {
            let Some(handle) = cx
                .active_window()
                .or_else(|| cx.windows().into_iter().next())
            else {
                return;
            };
            // 主窗口不因 cmd-w 关闭（IDEA 语义）：视图层有 tab 可关时已消费不冒泡，
            // 冒到这里说明无 tab 可关；误关主窗会丢弃全部会话 / 查询稿，代价过高。
            // 主窗关闭走 cmd-q 或系统关闭按钮；非主窗（抽屉等浮层）照常关闭
            if cx
                .try_global::<MainWindowGlobal>()
                .is_some_and(|g| g.0 == handle)
            {
                return;
            }
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        });

        cx.bind_keys([
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-1", SelectTool1, None),
            KeyBinding::new("secondary-2", SelectTool2, None),
            KeyBinding::new("secondary-3", SelectTool3, None),
            KeyBinding::new("secondary-4", SelectTool4, None),
            KeyBinding::new("ctrl-tab", CycleSection, None),
            KeyBinding::new("ctrl-shift-tab", CycleSectionReverse, None),
            KeyBinding::new("secondary-enter", RunQuery, None),
            KeyBinding::new("secondary-shift-enter", RunStatementAtCursor, None),
            KeyBinding::new("secondary-t", NewQueryTab, None),
            KeyBinding::new("secondary-w", CloseTab, None),
            KeyBinding::new("secondary-f", FindInResults, None),
            KeyBinding::new("secondary-shift-f", FormatSql, None),
            KeyBinding::new("secondary-shift-e", ExplainQuery, None),
            KeyBinding::new("secondary-e", ToggleSqlEditor, None),
            KeyBinding::new("secondary-enter", RunMongoQuery, Some("MongoQueryTab")),
            KeyBinding::new("secondary-t", NewMongoQueryTab, Some("MongoQueryPanel")),
            KeyBinding::new("secondary-shift-f", FormatMongoJson, Some("MongoQueryTab")),
            KeyBinding::new("secondary-e", ToggleMongoEditor, Some("MongoQueryPanel")),
            KeyBinding::new("secondary-e", ToggleRedisConsole, Some("RedisSession")),
            KeyBinding::new("secondary-k", FocusCommitMessage, Some("VcsView")),
            KeyBinding::new("secondary-enter", CommitNow, Some("VcsView")),
            KeyBinding::new("secondary-shift-k", PushNow, Some("VcsView")),
            KeyBinding::new("secondary-t", PullNow, Some("VcsView")),
            KeyBinding::new("secondary-r", RefreshWorkspace, Some("VcsView")),
            KeyBinding::new("secondary-s", SaveProjectFile, Some("VcsView")),
            KeyBinding::new("secondary-shift-h", ToggleHistoryPane, Some("VcsView")),
            KeyBinding::new("secondary-t", NewSshTerminal, Some("SshWorkspace")),
            KeyBinding::new("secondary-w", CloseSshTerminal, Some("SshWorkspace")),
            KeyBinding::new("secondary-w", CloseSshTerminal, Some("Terminal")),
            KeyBinding::new("secondary-r", RefreshSftp, Some("SshWorkspace")),
        ]);

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        cx.bind_keys([
            KeyBinding::new("secondary-f", FocusClipSearch, Some("ClipboardView")),
            KeyBinding::new("enter", CopySelectedClip, Some("ClipboardView")),
            KeyBinding::new("delete", DeleteSelectedClip, Some("ClipboardView")),
            KeyBinding::new("backspace", DeleteSelectedClip, Some("ClipboardView")),
            KeyBinding::new("down", SelectNextClip, Some("ClipboardView")),
            KeyBinding::new("up", SelectPrevClip, Some("ClipboardView")),
        ]);

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(svc) = deps.clipboard_service.clone() {
            let cleanup_service = svc.clone();
            cx.spawn(async move |_| {
                if let Err(e) = cleanup_service.cleanup_orphans().await {
                    tracing::warn!(error = %e, "clipboard orphan cleanup failed");
                }
            })
            .detach();
            let preload_service = svc.clone();
            cx.spawn(async move |_| preload_service.preload().await)
                .detach();
            spawn_clipboard_capture(svc.clone(), cx);
            spawn_clipboard_hotkey(svc, deps.registry.clone(), cx);
        }

        let log_path_for_open = log_path.clone();
        cx.on_action(move |_: &OpenLogDir, cx: &mut App| {
            let Some(dir) = log_path_for_open
                .as_ref()
                .and_then(|path| path.parent())
                .map(std::path::Path::to_path_buf)
            else {
                warn!("log file unavailable");
                return;
            };
            cx.spawn(async move |_| {
                let result = ramag_app::run_blocking(move || {
                    open_path_in_file_manager(&dir).map_err(|error| {
                        ramag_domain::error::DomainError::Other(format!(
                            "打开日志目录失败：{error}"
                        ))
                    })
                })
                .await;
                if let Err(error) = result {
                    warn!(error = %error, "open log directory failed");
                }
            })
            .detach();
        });

        cx.on_action(|_: &OpenFeedbackIssue, cx: &mut App| {
            cx.open_url(FEEDBACK_ISSUE_URL);
        });

        cx.set_menus(vec![
            Menu {
                name: "Ramag".into(),
                items: vec![MenuItem::action("退出 Ramag", Quit)],
                disabled: false,
            },
            Menu {
                name: "帮助".into(),
                items: vec![
                    MenuItem::action("查看日志", OpenLogDir),
                    MenuItem::action("反馈问题", OpenFeedbackIssue),
                ],
                disabled: false,
            },
        ]);

        open_main_window(deps.clone(), cx);
        if !cfg!(debug_assertions)
            && let Some(service) = deps.update_service.clone()
        {
            spawn_update_check(service, cx);
        }
    });
    info!("application stopped");
}

fn spawn_update_check(service: Arc<UpdateService>, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_secs(3))
            .await;
        let result = service.check(false).await;
        cx.update(|cx| match result {
            Ok(result) => sync_update_indicator(&result, cx),
            Err(error) => {
                warn!(error = %error, "automatic update check failed");
            }
        });
    })
    .detach();
}

fn confirm_ssh_host(prompt: &str) -> bool {
    let description = if prompt.trim().is_empty() {
        "OpenSSH 请求确认远程主机指纹。请仅在你确认目标服务器身份后继续。"
    } else {
        prompt
    };
    matches!(
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("确认 SSH 主机指纹")
            .set_description(description)
            .set_buttons(rfd::MessageButtons::YesNo)
            .show(),
        rfd::MessageDialogResult::Yes
    )
}

#[cfg(test)]
mod tests {
    use super::{MainWindowOpenGate, build_tool_registry};

    #[test]
    fn main_window_open_gate_coalesces_repeated_requests() {
        let mut gate = MainWindowOpenGate::default();

        assert!(gate.try_begin());
        assert!(!gate.try_begin());
        gate.finish();
        assert!(gate.try_begin());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn clipboard_capture_retry_backoff_is_bounded() {
        use super::{CAPTURE_INTERVAL, CAPTURE_MAX_RETRY_INTERVAL, next_capture_retry_interval};

        let mut interval = CAPTURE_INTERVAL;
        assert_eq!(
            next_capture_retry_interval(interval),
            CAPTURE_INTERVAL.saturating_mul(2)
        );
        for _ in 0..16 {
            interval = next_capture_retry_interval(interval);
        }
        assert_eq!(interval, CAPTURE_MAX_RETRY_INTERVAL);
        assert_eq!(
            next_capture_retry_interval(interval),
            CAPTURE_MAX_RETRY_INTERVAL
        );
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn clipboard_tool_is_registered_last() {
        let ids = build_tool_registry()
            .list()
            .into_iter()
            .map(|tool| tool.meta().id.clone())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["dbclient", "vcs", "ssh", "clipboard"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clipboard_tool_is_not_registered_on_linux() {
        let ids = build_tool_registry()
            .list()
            .into_iter()
            .map(|tool| tool.meta().id.clone())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["dbclient", "vcs", "ssh"]);
    }
}
