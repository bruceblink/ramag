//! 主入口：tracing → 装配数据层 → 注册 Tool → 启动 GPUI App → 打开主窗口

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod logging;
#[cfg(target_os = "windows")]
mod single_instance;
#[cfg(target_os = "windows")]
mod tray;
mod window_layout;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    Action, App, Bounds, KeyBinding, Menu, MenuItem, Subscription, TitlebarOptions, WindowBounds,
    WindowKind, WindowOptions, prelude::*, px, size,
};
use gpui_component::Root;
use ramag_app::{ClipboardService, ConnectionService, MongoService, RedisService, ToolRegistry};
use ramag_domain::traits::{ClipboardDriver, DocDriver, Driver, GitDriver, KvDriver, Storage};
use ramag_infra_clipboard::{HotkeyListener, PlatformClipboardDriver, foreground_display_index};
use ramag_infra_git::GitDriverImpl;
use ramag_infra_mongodb::MongoDriver;
use ramag_infra_mysql::MysqlDriver;
use ramag_infra_postgres::PostgresDriver;
use ramag_infra_redis::RedisDriver;
use ramag_infra_storage::RedbStorage;
use ramag_tool_clipboard::{
    ClipboardTool, CopySelectedClip, DeleteSelectedClip, FocusClipSearch, SelectNextClip,
    SelectPrevClip, create_clipboard_drawer, create_clipboard_view,
};
use ramag_tool_dbclient::{
    DbClientTool, ExplainQuery, FindInResults, FormatSql, NewQueryTab, RunQuery,
    RunStatementAtCursor, ToggleRedisConsole, ToggleSqlEditor, create_dbclient_view,
};
use ramag_tool_mongodb::{FormatMongoJson, NewMongoQueryTab, RunMongoQuery, ToggleMongoEditor};
use ramag_tool_vcs::{
    CommitNow, FocusCommitMessage, PullNow, PushNow, RefreshWorkspace, SaveProjectFile,
    ToggleHistoryPane, VcsTool, create_vcs_view,
};
use ramag_ui::{
    CloseTab, CycleSection, CycleSectionReverse, HomeEvent, HomeView, NavTarget, RamagAssets,
    SelectTool1, SelectTool2, SelectTool3, SettingsView, Shell, StorageGlobal, init_theme,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::window_layout::{drawer_bounds, preferred_display};

/// 绑定跨平台退出动作和原生菜单
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct Quit;

/// 帮助菜单：快捷键一览弹窗
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct ShowShortcuts;

/// 帮助菜单：关于（版本 + 日志路径）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct ShowAbout;

/// 帮助菜单：复制诊断信息（版本 / 平台 / 日志路径）到剪贴板，便于反馈
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct CopyDiagnostics;

/// 帮助菜单：在系统文件管理器中打开日志目录
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct OpenLogDir;

// 原生「编辑」菜单项：paired action 仅占位，macOS 由 os_action 角色经响应链处理标准编辑命令
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct EditUndo;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct EditRedo;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct EditCut;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct EditCopy;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct EditPaste;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
struct EditSelectAll;

/// 在系统文件管理器中打开目录（打开日志目录用）
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

/// 诊断信息文本：版本 + 平台 + 日志路径（复制 / 关于弹窗共用）
fn diagnostics_text(log_path: &Option<std::path::PathBuf>) -> String {
    let log_line = log_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（日志未启用）".to_string());
    format!(
        "Ramag {}\n平台：{} {}\n日志：{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        log_line
    )
}

/// 全部工具服务与存储的共享句柄；主窗口重建（on_reopen / 托盘 / 单实例激活）复用
#[derive(Clone)]
struct AppDeps {
    registry: Arc<ToolRegistry>,
    conn_service: Arc<ConnectionService>,
    redis_service: Arc<RedisService>,
    mongo_service: Arc<MongoService>,
    clipboard_service: Arc<ClipboardService>,
    storage: Arc<dyn Storage>,
}

/// 最近一次打开的主窗口句柄；托盘 / 单实例激活时优先前台化它而非重开
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

/// 唤起主窗口：已有则前台激活（含最小化恢复），已关闭则重开
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
    let log_path = logging::init();
    info!(version = env!("CARGO_PKG_VERSION"), "ramag launching");

    // 单实例：已有实例在跑则通知其唤起主窗口后静默退出（避免 redb 文件锁报错）；
    // macOS 由系统 LaunchServices 保证 .app 单实例，无需自建
    #[cfg(target_os = "windows")]
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
            error!(error = %e, "failed to initialize data layer");
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

    // Redis 共用同一 storage
    let redis_service: Arc<RedisService> = build_redis_service(storage.clone());
    // MongoDB 共用同一 storage
    let mongo_service: Arc<MongoService> = build_mongo_service(storage.clone());
    // 剪贴板共用同一 storage（历史与设置走同一份加密 redb）
    let clipboard_service: Arc<ClipboardService> = build_clipboard_service(storage.clone());

    // 主题偏好。"dark" 用暗色，其余（含旧版 "system" 残值）默认浅色
    let startup_preferences = read_preferences(&storage, &["theme_mode"]);
    let initial_pref = startup_preferences.get("theme_mode").cloned();

    // 剪贴板总开关决定工具入口可见性；启动同步读取，避免「恢复上次工具」误入已隐藏的剪贴板
    let clipboard_enabled = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(clipboard_service.prime_capture_enabled()),
        Err(error) => {
            warn!(error = %error, "read clipboard settings failed; keep tool visible");
            true
        }
    };

    let registry = build_tool_registry();
    registry.set_enabled(ClipboardTool::ID, clipboard_enabled);
    info!(tool_count = registry.count(), "tools registered");

    let deps = AppDeps {
        registry,
        conn_service,
        redis_service,
        mongo_service,
        clipboard_service,
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
        // 主题两态（浅 / 深），默认浅色；开窗前初始化避免首帧闪烁
        init_theme(initial_pref.as_deref(), cx);
        // storage 注入 cx 全局，ActivityBar 切主题用它持久化
        cx.set_global(StorageGlobal(deps.storage.clone()));
        cx.activate(true);

        // 必须先 bind_keys 把退出快捷键绑到 Quit，原生菜单项才会显示快捷键
        cx.on_action(|_: &Quit, cx| cx.quit());

        // 退出时关闭全部 SSH 隧道子进程，避免残留孤儿 ssh 占用端口
        cx.on_app_quit(|_| async {
            ramag_infra_tunnel::shutdown_all();
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
            // 工具切换：主修饰键+1/2/3 跳到第 N 个工具；Ctrl+Tab 循环区段（Shell 处理）
            KeyBinding::new("secondary-1", SelectTool1, None),
            KeyBinding::new("secondary-2", SelectTool2, None),
            KeyBinding::new("secondary-3", SelectTool3, None),
            KeyBinding::new("ctrl-tab", CycleSection, None),
            KeyBinding::new("ctrl-shift-tab", CycleSectionReverse, None),
            // dbclient (MySQL / PG) 视图的快捷键（context=QueryPanel/QueryTab 见 dbclient 视图实现）
            KeyBinding::new("secondary-enter", RunQuery, None),
            KeyBinding::new("secondary-shift-enter", RunStatementAtCursor, None),
            KeyBinding::new("secondary-t", NewQueryTab, None),
            KeyBinding::new("secondary-w", CloseTab, None),
            KeyBinding::new("secondary-f", FindInResults, None),
            KeyBinding::new("secondary-shift-f", FormatSql, None),
            KeyBinding::new("secondary-shift-e", ExplainQuery, None),
            KeyBinding::new("secondary-e", ToggleSqlEditor, None),
            // MongoDB 视图的快捷键，用 KeyContext 限定（焦点在 Mongo 视图时优先）
            KeyBinding::new("secondary-enter", RunMongoQuery, Some("MongoQueryTab")),
            KeyBinding::new("secondary-t", NewMongoQueryTab, Some("MongoQueryPanel")),
            KeyBinding::new("secondary-shift-f", FormatMongoJson, Some("MongoQueryTab")),
            KeyBinding::new("secondary-e", ToggleMongoEditor, Some("MongoQueryPanel")),
            // Redis 命令行控制台：主修饰键+E 在会话上下文切换显隐
            KeyBinding::new("secondary-e", ToggleRedisConsole, Some("RedisSession")),
            // VCS 视图快捷键（context=VcsView，焦点在 VCS 视图时优先于上面的 None context 绑定）
            KeyBinding::new("secondary-k", FocusCommitMessage, Some("VcsView")),
            KeyBinding::new("secondary-enter", CommitNow, Some("VcsView")),
            KeyBinding::new("secondary-shift-k", PushNow, Some("VcsView")),
            KeyBinding::new("secondary-t", PullNow, Some("VcsView")),
            KeyBinding::new("secondary-r", RefreshWorkspace, Some("VcsView")),
            KeyBinding::new("secondary-s", SaveProjectFile, Some("VcsView")),
            KeyBinding::new("secondary-shift-h", ToggleHistoryPane, Some("VcsView")),
            // 剪贴板视图快捷键（KeyContext=ClipboardView，焦点在剪贴板视图时生效）
            KeyBinding::new("secondary-f", FocusClipSearch, Some("ClipboardView")),
            KeyBinding::new("enter", CopySelectedClip, Some("ClipboardView")),
            KeyBinding::new("delete", DeleteSelectedClip, Some("ClipboardView")),
            KeyBinding::new("backspace", DeleteSelectedClip, Some("ClipboardView")),
            KeyBinding::new("down", SelectNextClip, Some("ClipboardView")),
            KeyBinding::new("up", SelectPrevClip, Some("ClipboardView")),
        ]);

        // 启动时清理孤儿媒体文件（崩溃 / 库磁盘不一致残留）
        {
            let svc = deps.clipboard_service.clone();
            cx.spawn(async move |_| {
                if let Err(e) = svc.cleanup_orphans().await {
                    tracing::warn!(error = %e, "clipboard orphan cleanup failed");
                }
            })
            .detach();
        }
        // 预热窗口缓存：解密最近 N 条入内存，让首次唤起抽屉即同步带满内容
        {
            let svc = deps.clipboard_service.clone();
            cx.spawn(async move |_| svc.preload().await).detach();
        }
        // App 级剪贴板采集循环：独立于窗口生死（Windows 托盘常驻期间同样持续记录）
        spawn_clipboard_capture(deps.clipboard_service.clone(), cx);
        // 平台全局热键（macOS Command+Shift+V / Windows Ctrl+Shift+V）唤起抽屉；
        // 同一循环把总开关同步到工具入口可见性
        spawn_clipboard_hotkey(deps.clipboard_service.clone(), deps.registry.clone(), cx);

        // 帮助入口：快捷键一览 / 关于（active window 上开 dialog；无窗口时静默）
        cx.on_action(|_: &ShowShortcuts, cx: &mut App| {
            if let Some(handle) = cx.active_window() {
                let _ = handle.update(cx, |_, window, cx| open_shortcuts_dialog(window, cx));
            }
        });
        let log_path_for_about = log_path.clone();
        cx.on_action(move |_: &ShowAbout, cx: &mut App| {
            let log_path = log_path_for_about.clone();
            if let Some(handle) = cx.active_window() {
                let _ = handle.update(cx, move |_, window, cx| {
                    open_about_dialog(log_path, window, cx)
                });
            }
        });
        // 复制诊断信息：版本 / 平台 / 日志路径写入剪贴板
        let log_path_for_diag = log_path.clone();
        cx.on_action(move |_: &CopyDiagnostics, cx: &mut App| {
            let text = diagnostics_text(&log_path_for_diag);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        });
        // 打开日志目录：在系统文件管理器中定位日志文件所在目录
        let log_path_for_open = log_path.clone();
        cx.on_action(move |_: &OpenLogDir, cx: &mut App| {
            let Some(dir) = log_path_for_open
                .as_ref()
                .and_then(|path| path.parent())
                .map(std::path::Path::to_path_buf)
            else {
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
                    tracing::warn!(error = %error, "open log dir failed");
                }
            })
            .detach();
        });

        cx.set_menus(vec![
            Menu {
                name: "Ramag".into(),
                items: vec![MenuItem::action("退出 Ramag", Quit)],
                disabled: false,
            },
            // 原生编辑菜单：os_action 角色让 macOS 标准编辑命令（撤销 / 剪切 / 复制 / 粘贴 / 全选）
            // 在菜单可见且沿响应链生效，输入框内到处可用
            Menu {
                name: "编辑".into(),
                items: vec![
                    MenuItem::os_action("撤销", EditUndo, gpui::OsAction::Undo),
                    MenuItem::os_action("重做", EditRedo, gpui::OsAction::Redo),
                    MenuItem::separator(),
                    MenuItem::os_action("剪切", EditCut, gpui::OsAction::Cut),
                    MenuItem::os_action("复制", EditCopy, gpui::OsAction::Copy),
                    MenuItem::os_action("粘贴", EditPaste, gpui::OsAction::Paste),
                    MenuItem::os_action("全选", EditSelectAll, gpui::OsAction::SelectAll),
                ],
                disabled: false,
            },
            Menu {
                name: "帮助".into(),
                items: vec![
                    MenuItem::action("快捷键一览", ShowShortcuts),
                    MenuItem::separator(),
                    MenuItem::action("复制诊断信息", CopyDiagnostics),
                    MenuItem::action("打开日志目录", OpenLogDir),
                    MenuItem::separator(),
                    MenuItem::action("关于 Ramag", ShowAbout),
                ],
                disabled: false,
            },
        ]);

        open_main_window(deps.clone(), cx);
    });
}

/// 采集间隔。两平台统一轮询系统剪贴板序列号，仅在变化时读取内容。
const CAPTURE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);
/// 持久化或系统剪贴板持续失败时指数退避，避免每 400ms 重复占用 CPU 与刷日志。
const CAPTURE_MAX_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

fn next_capture_retry_interval(current: std::time::Duration) -> std::time::Duration {
    current.saturating_mul(2).min(CAPTURE_MAX_RETRY_INTERVAL)
}

/// App 级采集循环：启动预热一次设置；之后仅在 changeCount 变化时读取内存快照并处理。
/// driver 读取在前台 executor 执行，满足 macOS AppKit 的主线程约束。
fn spawn_clipboard_capture(service: Arc<ClipboardService>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut last_count = service.driver().change_count();
        let mut poll_interval = CAPTURE_INTERVAL;
        service.load_settings().await;
        loop {
            cx.background_executor().timer(poll_interval).await;
            let count = service.driver().change_count();
            if count == last_count {
                poll_interval = CAPTURE_INTERVAL;
                continue;
            }
            let settings = service.capture_settings_snapshot().await;
            match service.capture_tick(&settings).await {
                Ok(_) => {
                    last_count = count;
                    poll_interval = CAPTURE_INTERVAL;
                }
                Err(e) => {
                    // 读取失败时保留旧序列号，下个周期重试同一份内容，避免 Windows 剪贴板占用导致漏采。
                    poll_interval = next_capture_retry_interval(poll_interval);
                    tracing::warn!(
                        error = %e,
                        retry_ms = poll_interval.as_millis(),
                        "clipboard capture tick failed"
                    );
                }
            }
        }
    })
    .detach();
}

/// 热键轮询间隔：channel 有事件即触发，间隔短以保证唤起手感
const HOTKEY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

/// 剪贴板总开关 → 工具入口可见性；变化时刷新全部窗口，ActivityBar 与首页即时增删入口
fn sync_clipboard_tool_visibility(registry: &Arc<ToolRegistry>, enabled: bool, cx: &mut App) {
    if registry.set_enabled(ClipboardTool::ID, enabled) {
        cx.refresh_windows();
    }
}

/// 注册全局热键并轮询：触发切换抽屉；并在每拍检测失焦自动隐藏（点击外部即关）。
/// 热键随「启用剪贴板」总开关动态注册/注销，关闭即释放平台全局热键，
/// 并同步工具入口可见性（侧边栏 / 首页卡片随开关增删）。
/// 注册失败（缺权限等）仅记日志，不影响其余功能
fn spawn_clipboard_hotkey(
    service: Arc<ClipboardService>,
    registry: Arc<ToolRegistry>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        // 启动读持久化设置：总开关关闭则不注册，避免抢占平台全局热键
        let mut enabled = service.prime_capture_enabled().await;
        cx.update(|cx| sync_clipboard_tool_visibility(&registry, enabled, cx));
        let mut alternate = service.alternate_hotkey();
        let mut listener = if enabled {
            let l = HotkeyListener::register_clipboard_hotkey(alternate);
            if l.is_none() {
                error!("global hotkey register failed; clipboard drawer disabled");
            }
            // 状态上报给设置面板展示（失败常见原因：组合键被其它应用占用）
            service.set_hotkey_state(if l.is_some() {
                ramag_app::HotkeyState::Registered
            } else {
                ramag_app::HotkeyState::Failed
            });
            l
        } else {
            service.set_hotkey_state(ramag_app::HotkeyState::Disabled);
            None
        };

        let mut drawer: Option<gpui::AnyWindowHandle> = None;
        // 抽屉是否曾真正激活过：避免刚打开（尚未激活）就被失焦逻辑误关
        let mut was_active = false;
        loop {
            cx.background_executor().timer(HOTKEY_POLL_INTERVAL).await;

            // 总开关或热键组合变化 → 动态注册/注销热键 + 同步工具入口可见性
            let now_enabled = service.capture_enabled();
            let now_alternate = service.alternate_hotkey();
            if now_enabled != enabled || (now_enabled && now_alternate != alternate) {
                enabled = now_enabled;
                alternate = now_alternate;
                cx.update(|cx| sync_clipboard_tool_visibility(&registry, enabled, cx));
                // 先置 None 触发 Drop 注销旧热键（切换组合时避免新旧并存）
                listener = None;
                if enabled {
                    listener = HotkeyListener::register_clipboard_hotkey(alternate);
                    if listener.is_none() {
                        error!("global hotkey re-register failed");
                    }
                    service.set_hotkey_state(if listener.is_some() {
                        ramag_app::HotkeyState::Registered
                    } else {
                        ramag_app::HotkeyState::Failed
                    });
                } else {
                    service.set_hotkey_state(ramag_app::HotkeyState::Disabled);
                    // 关闭残留抽屉：热键已注销，否则无法再 toggle 关闭
                    if let Some(handle) = drawer.take() {
                        let _ = cx
                            .update(|cx| handle.update(cx, |_, window, _| window.remove_window()));
                        was_active = false;
                    }
                }
            }

            // 失焦自动隐藏：曾激活过又失去激活态 = 用户点了别处
            if let Some(handle) = drawer {
                let active =
                    cx.update(|cx| handle.update(cx, |_, window, _| window.is_window_active()));
                match active {
                    Ok(true) => was_active = true,
                    Ok(false) if was_active => {
                        let _ = cx
                            .update(|cx| handle.update(cx, |_, window, _| window.remove_window()));
                        drawer = None;
                        was_active = false;
                    }
                    // 窗口已被系统或用户关闭：立即丢弃失效句柄，否则下一次热键只会清句柄，
                    // 需要按第二次才重新打开。
                    Err(_) => {
                        drawer = None;
                        was_active = false;
                    }
                    Ok(false) => {}
                }
            }

            // 采集关闭（无 listener）→ 跳过热键轮询
            let Some(listener) = &listener else {
                continue;
            };
            if !listener.poll() {
                continue;
            }
            // 已打开 → 关闭（toggle）
            if let Some(handle) = drawer.take() {
                let _ = cx.update(|cx| handle.update(cx, |_, window, _| window.remove_window()));
                was_active = false;
                continue;
            }
            // 未打开 → 唤起：记录前台应用后开抽屉
            let svc = service.clone();
            drawer = cx.update(|cx| open_drawer_window(svc, cx));
            was_active = false;
        }
    })
    .detach();
}

/// 在前台应用所在显示器底部打开满宽 Floating 抽屉窗口。
/// 用 Floating（非 PopUp）+ 激活 app，搜索框输入法（中文）才能工作；可见区贴底避开 Dock
fn open_drawer_window(
    service: Arc<ClipboardService>,
    cx: &mut App,
) -> Option<gpui::AnyWindowHandle> {
    let display_index = foreground_display_index();
    let activation_target = service.driver().activation_target();

    let display = preferred_display(cx, display_index)?;
    let bounds = drawer_bounds(display.visible_bounds());

    // PopUp + 激活 app：PopUp 自带 CanJoinAllSpaces（全屏 Space 也能弹出）；
    // cx.activate 让 app active，搜索框输入法（中文）方可工作；粘贴时再激活回原应用
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            display_id: Some(display.id()),
            titlebar: None,
            kind: WindowKind::PopUp,
            is_movable: false,
            focus: true,
            show: true,
            ..Default::default()
        },
        move |window, cx| {
            let drawer = create_clipboard_drawer(service, activation_target, window, cx);
            let root = cx.new(|cx| Root::new(drawer, window, cx));
            // Windows：cx.activate(true) 是 no-op，须窗口级 activate_window（内部 SetForegroundWindow）
            // 抽屉才能抢到前台，搜索框中文输入法 / 粘贴才正常；macOS 同样受益
            window.activate_window();
            root
        },
    );
    cx.activate(true);
    match result {
        Ok(handle) => Some(handle.into()),
        Err(e) => {
            error!(error = %e, "open drawer window failed");
            None
        }
    }
}

/// init / on_reopen / 托盘唤起共用；成功后把窗口句柄记入 MainWindowGlobal
fn open_main_window(deps: AppDeps, cx: &mut App) {
    if !begin_main_window_open(cx) {
        return;
    }
    let window_preferences = read_preferences(
        &deps.storage,
        &["last_tool", ramag_ui::WindowBoundsPref::PREF_KEY],
    );
    // 恢复上次停留的工具（重启不回炉 Home）；registry 校验防 pref 残留失效 id
    let last_tool = window_preferences
        .get("last_tool")
        .filter(|tool| !tool.is_empty())
        .cloned();
    // 恢复上次窗口位置尺寸；无记录默认最大化（centered 作取消最大化的复位 bounds）。
    // 尺寸下限对齐 window_min_size，防坏数据 / 换显示器造出不可用窗口
    let saved_bounds = window_preferences
        .get(ramag_ui::WindowBoundsPref::PREF_KEY)
        .and_then(|json| match ramag_ui::WindowBoundsPref::parse(json) {
            Ok(pref) => Some(pref),
            Err(error) => {
                warn!(error, "ignore invalid saved window bounds");
                None
            }
        });
    let AppDeps {
        registry,
        conn_service,
        redis_service,
        mongo_service,
        clipboard_service,
        storage,
    } = deps;
    let fallback = Bounds::centered(None, size(px(1200.0), px(780.0)), cx);
    let window_bounds = match &saved_bounds {
        Some(p) => {
            let b = Bounds::new(
                gpui::point(px(p.x), px(p.y)),
                size(px(p.w.max(800.0)), px(p.h.max(500.0))),
            );
            // 校验保存的位置是否仍落在某个显示器内：外接屏拔除后旧坐标可能整体在屏幕外，
            // 恢复出去将无法拖回。要求标题栏区域（顶部中点）落在某显示器内，保证可拖动，
            // 否则丢弃位置、回退居中最大化
            let title_pt = gpui::point(b.origin.x + b.size.width / 2.0, b.origin.y + px(16.0));
            let on_screen = cx.displays().iter().any(|d| d.bounds().contains(&title_pt));
            if !on_screen {
                info!("saved window position is off-screen; falling back to centered");
                WindowBounds::Maximized(fallback)
            } else if p.maximized {
                WindowBounds::Maximized(b)
            } else {
                WindowBounds::Windowed(b)
            }
        }
        None => WindowBounds::Maximized(fallback),
    };

    cx.spawn(async move |cx| {
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                window_min_size: Some(size(px(800.0), px(500.0))),
                // 原生标题栏需 appears_transparent=false，否则失去双击 zoom 命中区
                titlebar: Some(TitlebarOptions {
                    title: if cfg!(target_os = "windows") {
                        Some("Ramag".into())
                    } else {
                        None
                    },
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                ..Default::default()
            },
            move |window, cx| {
                let home_view = cx.new(|_| HomeView::new(registry.clone()));

                let dbclient_view = create_dbclient_view(
                    conn_service.clone(),
                    redis_service.clone(),
                    mongo_service.clone(),
                    window,
                    cx,
                );

                let git_driver: Arc<dyn GitDriver> = Arc::new(GitDriverImpl::new());
                let vcs_view = create_vcs_view(git_driver, storage.clone(), window, cx);

                let clipboard_view = create_clipboard_view(clipboard_service.clone(), window, cx);
                let settings_view = cx.new(|cx| SettingsView::new(clipboard_service.clone(), cx));

                let shell = cx.new(|cx| {
                    let mut shell = Shell::new(registry.clone(), window, cx);
                    shell.set_home_view(home_view.clone().into());
                    shell.set_settings_view(settings_view.clone().into());
                    shell.register_tool_view(DbClientTool::ID, dbclient_view.clone().into());
                    shell.register_tool_view(VcsTool::ID, vcs_view.into());
                    shell.register_tool_view(ClipboardTool::ID, clipboard_view.clone().into());

                    let home_subscription: Subscription = cx.subscribe_in(
                        &home_view,
                        window,
                        move |this: &mut Shell, _, event: &HomeEvent, window, cx| match event {
                            HomeEvent::OpenTool(tool_id) => {
                                this.navigate_to(NavTarget::Tool(tool_id.clone()), window, cx);
                            }
                        },
                    );
                    shell.retain_subscription(home_subscription);

                    shell
                });

                // 恢复上次工具视图（工具已被移除则忽略，留在 Home）
                if let Some(tool_id) = last_tool.clone().filter(|t| registry.find(t).is_some()) {
                    shell.update(cx, |s, cx| {
                        s.navigate_to(NavTarget::Tool(tool_id), window, cx);
                    });
                }

                cx.new(|cx| Root::new(shell, window, cx))
            },
        );
        match result {
            Ok(handle) => {
                cx.update(|cx| {
                    finish_main_window_open(cx);
                    cx.set_global(MainWindowGlobal(handle.into()));
                });
            }
            Err(err) => {
                cx.update(finish_main_window_open);
                error!(error = %err, "open window failed");
            }
        }
    })
    .detach();
}

/// 托盘事件轮询：Open 唤起主窗口；Quit 先删图标（防任务栏残影）再退出
#[cfg(target_os = "windows")]
fn spawn_tray_loop(
    tray: std::rc::Rc<std::cell::RefCell<Option<tray::TrayIcon>>>,
    deps: AppDeps,
    cx: &mut App,
) {
    const TRAY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(TRAY_POLL_INTERVAL).await;
            let event = tray.borrow().as_ref().and_then(|t| t.poll());
            match event {
                Some(tray::TrayEvent::Open) => {
                    cx.update(|cx| reveal_main_window(&deps, cx));
                }
                Some(tray::TrayEvent::Quit) => {
                    drop(tray.borrow_mut().take());
                    cx.update(|cx| cx.quit());
                    break;
                }
                None => {}
            }
        }
    })
    .detach();
}

/// 单实例激活轮询：后启进程置位命名事件 → 唤起主窗口
#[cfg(target_os = "windows")]
fn spawn_instance_activation(guard: single_instance::PrimaryGuard, deps: AppDeps, cx: &mut App) {
    const ACTIVATE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(ACTIVATE_POLL_INTERVAL).await;
            if guard.poll_activate() {
                cx.update(|cx| reveal_main_window(&deps, cx));
            }
        }
    })
    .detach();
}

/// 注册 SQL 类 driver 到 `HashMap<DriverKind, Arc<dyn Driver>>`，按 `config.driver` 分发；Redis 走独立 service
fn build_connection_service() -> anyhow::Result<(Arc<ConnectionService>, Arc<dyn Storage>)> {
    use ramag_domain::entities::DriverKind;
    use std::collections::HashMap;

    let mut drivers: HashMap<DriverKind, Arc<dyn Driver>> = HashMap::new();
    drivers.insert(DriverKind::Mysql, Arc::new(MysqlDriver::new()));
    drivers.insert(DriverKind::Postgres, Arc::new(PostgresDriver::new()));

    let storage_impl =
        RedbStorage::open_default().map_err(|e| anyhow::anyhow!("初始化 redb 存储失败: {e}"))?;
    info!(path = %storage_impl.path().display(), "storage opened");
    let storage: Arc<dyn Storage> = Arc::new(storage_impl);

    let svc = Arc::new(ConnectionService::new(drivers, storage.clone()));
    Ok((svc, storage))
}

/// 快捷键一览弹窗（静态内容；⌘=macOS / Ctrl=Windows 同位）
fn open_shortcuts_dialog(window: &mut gpui::Window, cx: &mut App) {
    use gpui::{ParentElement as _, Styled as _};
    use gpui_component::WindowExt as _;
    const ROWS: &[(&str, &str)] = &[
        (
            "⌘/Ctrl ⇧ V",
            "全局唤起剪贴板抽屉（系统级，应用不在前台也可）",
        ),
        (
            "⌘/Ctrl Q · ⌘/Ctrl W",
            "退出应用 · 关闭当前标签（主窗口不因 W 关闭）",
        ),
        ("Ctrl Tab · Ctrl ⇧ Tab", "向前 / 向后切换首页、工具与设置"),
        ("⌘/Ctrl 1 / 2 / 3", "直接切换到第 1 / 2 / 3 个工具"),
        ("⌘/Ctrl Enter", "DB：运行查询 ｜ VCS：提交（提交框聚焦时）"),
        ("⌘/Ctrl ⇧ Enter", "DB：运行光标处语句"),
        ("⌘/Ctrl T", "DB：新建查询标签 ｜ VCS：Pull"),
        (
            "⌘/Ctrl E",
            "DB：显示 / 隐藏编辑器（SQL · Mongo · Redis 控制台）",
        ),
        ("⌘/Ctrl F", "DB：结果内查找 ｜ 剪贴板：聚焦搜索"),
        ("⌘/Ctrl ⇧ F", "DB：格式化 SQL / JSON"),
        ("⌘/Ctrl ⇧ E", "SQL：EXPLAIN 当前查询"),
        ("⌘/Ctrl K · ⌘/Ctrl ⇧ K", "VCS：聚焦提交输入 · Push"),
        ("⌘/Ctrl R · ⌘/Ctrl ⇧ H", "VCS：刷新工作区 · 底部历史面板"),
        (
            "Enter / Delete / ↑ ↓",
            "剪贴板：复制选中 / 删除选中 / 上下选择",
        ),
    ];
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(ramag_ui::closable_dialog_title(
                "shortcuts-dialog-close",
                "快捷键一览",
                |_, _| {},
            ))
            .close_button(false)
            .width(gpui::px(560.0))
            .content(move |content, _, cx| {
                let muted = gpui_component::ActiveTheme::theme(cx).muted_foreground;
                let mut col = gpui_component::v_flex().w_full().gap(gpui::px(6.0));
                for (keys, desc) in ROWS {
                    col = col.child(
                        gpui_component::h_flex()
                            .w_full()
                            .gap(gpui::px(12.0))
                            .child(
                                gpui::div()
                                    .w(gpui::px(170.0))
                                    .flex_none()
                                    .text_sm()
                                    .font_family("monospace")
                                    .child(*keys),
                            )
                            .child(
                                gpui::div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(*desc),
                            ),
                    );
                }
                content.child(col)
            })
    });
}

/// 关于弹窗：版本 + 日志路径（可复制排查）
fn open_about_dialog(
    log_path: Option<std::path::PathBuf>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    use gpui::{ParentElement as _, Styled as _};
    use gpui_component::WindowExt as _;
    let version = env!("CARGO_PKG_VERSION").to_string();
    let log_text = log_path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（日志文件不可用）".to_string());
    window.open_dialog(cx, move |dialog, _, _| {
        let version = version.clone();
        let log_text = log_text.clone();
        dialog
            .title(ramag_ui::closable_dialog_title(
                "about-dialog-close",
                "关于 Ramag",
                |_, _| {},
            ))
            .close_button(false)
            .width(gpui::px(520.0))
            .content(move |content, _, cx| {
                let muted = gpui_component::ActiveTheme::theme(cx).muted_foreground;
                content.child(
                    gpui_component::v_flex()
                        .w_full()
                        .gap(gpui::px(8.0))
                        .child(gpui::div().text_sm().child(format!("版本：{version}")))
                        .child(
                            gpui::div()
                                .text_sm()
                                .text_color(muted)
                                .child(format!("日志：{log_text}")),
                        )
                        .child(gpui::div().text_xs().text_color(muted).child(
                            "本地优先：连接密码经系统钥匙串主密钥 AES-GCM 加密落盘，剪贴板历史全量本地加密存储",
                        )),
                )
            })
    });
}

/// 启动期同步批量读取偏好。current-thread runtime 不创建后台工作线程；同一批 key 共用一次初始化。
fn read_preferences(
    storage: &Arc<dyn Storage>,
    keys: &[&'static str],
) -> HashMap<&'static str, String> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            warn!(error = %error, key_count = keys.len(), "create preference runtime failed");
            return HashMap::new();
        }
    };
    let mut preferences = HashMap::with_capacity(keys.len());
    for &key in keys {
        match runtime.block_on(storage.get_preference(key)) {
            Ok(Some(value)) => {
                preferences.insert(key, value);
            }
            Ok(None) => {}
            Err(error) => {
                warn!(error = %error, key, "read preference failed");
            }
        }
    }
    preferences
}

/// MySQL / Postgres / Redis 共用 DbClient 入口，driver 在表单选择器内
fn build_tool_registry() -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(DbClientTool::new()));
    registry.register(Arc::new(VcsTool::new()));
    registry.register(Arc::new(ClipboardTool::new()));
    registry
}

fn build_redis_service(storage: Arc<dyn Storage>) -> Arc<RedisService> {
    let driver: Arc<dyn KvDriver> = Arc::new(RedisDriver::new());
    Arc::new(RedisService::new(driver, storage))
}

fn build_mongo_service(storage: Arc<dyn Storage>) -> Arc<MongoService> {
    let driver: Arc<dyn DocDriver> = Arc::new(MongoDriver::new());
    Arc::new(MongoService::new(driver, storage))
}

fn build_clipboard_service(storage: Arc<dyn Storage>) -> Arc<ClipboardService> {
    let driver: Arc<dyn ClipboardDriver> = Arc::new(PlatformClipboardDriver::new());
    Arc::new(ClipboardService::new(driver, storage))
}

#[cfg(test)]
mod tests {
    use super::{
        CAPTURE_INTERVAL, CAPTURE_MAX_RETRY_INTERVAL, MainWindowOpenGate,
        next_capture_retry_interval,
    };

    #[test]
    fn main_window_open_gate_coalesces_repeated_requests() {
        let mut gate = MainWindowOpenGate::default();

        assert!(gate.try_begin());
        assert!(!gate.try_begin());
        gate.finish();
        assert!(gate.try_begin());
    }

    #[test]
    fn clipboard_capture_retry_backoff_is_bounded() {
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
}
