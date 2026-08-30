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
    AUTO_CHECK_INTERVAL, ClipboardService, ConnectionService, DataSyncGate, DataSyncService,
    KafkaService, MongoService, ObjectStorageService, RedisService, SshService,
    TOOL_ORDER_PREF_KEY, ToolRegistry, UpdateService,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ramag_domain::traits::ClipboardDriver;
use ramag_domain::traits::{
    DocDriver, Driver, GitDriver, JumpServerDriver, KafkaDriver, KvDriver, SshDriver, Storage,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ramag_infra_clipboard::{
    HotkeyEvent, HotkeyListener, PlatformClipboardDriver, foreground_display_index,
};
use ramag_infra_git::GitDriverImpl;
use ramag_infra_kafka::RdkafkaDriver;
use ramag_infra_mongodb::MongoDriver;
use ramag_infra_mysql::MysqlDriver;
use ramag_infra_postgres::PostgresDriver;
use ramag_infra_redis::RedisDriver;
use ramag_infra_ssh::{JumpServerHttpDriver, OpenSshDriver};
use ramag_infra_storage::RedbStorage;
use ramag_infra_update::GitHubUpdateDriver;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ramag_tool_clipboard::{
    ClipboardImageCache, ClipboardTool, SelectNextClip, SelectPrevClip,
    create_clipboard_drawer_with_cache, create_clipboard_view,
};
use ramag_tool_dbclient::{
    DbClientTool, ExplainQuery, FindInResults, FormatSql, NewQueryTab, RunQuery,
    RunStatementAtCursor, ToggleRedisConsole, ToggleSqlEditor, create_dbclient_view,
};
use ramag_tool_kafka::{KafkaTool, create_kafka_view};
use ramag_tool_mongodb::{FormatMongoJson, NewMongoQueryTab, RunMongoQuery, ToggleMongoEditor};
use ramag_tool_object_storage::{ObjectStorageTool, create_object_storage_view};
use ramag_tool_ssh::{CloseSshTerminal, NewSshTerminal, SshTool, create_ssh_view};
use ramag_tool_system::{SystemTool, create_system_view};
use ramag_tool_vcs::{CommitNow, PullNow, PushNow, ToggleHistoryPane, VcsTool, create_vcs_view};
use ramag_ui::{
    CloseTab, DATABASE_RESULT_SETTINGS_PREF_KEY, DATABASE_SEARCH_SETTINGS_PREF_KEY,
    FEEDBACK_ISSUE_URL, HomeEvent, HomeView, NavTarget, OpenRecentItems,
    REDIS_TREE_SETTINGS_PREF_KEY, RamagAssets, SYSTEM_SETTINGS_PREF_KEY, SettingsView, Shell,
    StorageGlobal, init_database_result_settings, init_database_search_settings,
    init_redis_tree_settings, init_system_settings, init_theme, sync_update_indicator,
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

fn main() {
    if let Some(exit_code) = ramag_infra_ssh::run_askpass_helper(confirm_ssh_host) {
        std::process::exit(exit_code);
    }

    let log_path = logging::init();
    info!(
        operation = "application_start",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        process_id = std::process::id(),
        debug = cfg!(debug_assertions),
        "application starting"
    );

    if let Err(error) = install_tls_crypto_provider() {
        error!(operation = "tls_provider_init", error = %error, "TLS crypto provider initialization failed");
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Ramag 启动失败")
            .set_description(format!("无法初始化安全网络组件：{error}"))
            .show();
        std::process::exit(1);
    }

    // 单实例：通知已有进程唤起主窗口后退出，避免 redb 锁冲突；macOS 由 LaunchServices 保证。
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let instance_guard = match single_instance::acquire() {
        single_instance::InstanceRole::Secondary => {
            info!(
                operation = "application_single_instance",
                role = "secondary",
                "another instance is running; asked it to reveal and exiting"
            );
            return;
        }
        single_instance::InstanceRole::Primary(guard) => guard,
    };

    let (conn_service, storage) = match build_connection_service() {
        Ok(pair) => pair,
        Err(e) => {
            error!(
                operation = "application_data_layer_init",
                error = %e,
                "data layer initialization failed"
            );
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
    let kafka_service: Arc<KafkaService> = build_kafka_service(storage.clone());
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
    let object_storage_service = match build_object_storage_service(storage.clone()) {
        Ok(service) => service,
        Err(error) => {
            error!(operation = "object_storage_init", error = %error, "object storage initialization failed");
            let _ = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Ramag 启动失败")
                .set_description(format!("无法初始化对象存储模块：{error}"))
                .show();
            std::process::exit(1);
        }
    };
    let update_service = build_update_service(storage.clone());

    // dark 使用深色主题；其余值（含旧 system）使用浅色主题。
    let startup_preferences = read_preferences(
        &storage,
        &[
            "theme_mode",
            DATABASE_RESULT_SETTINGS_PREF_KEY,
            DATABASE_SEARCH_SETTINGS_PREF_KEY,
            REDIS_TREE_SETTINGS_PREF_KEY,
            SYSTEM_SETTINGS_PREF_KEY,
            ramag_ui::shortcuts_dialog::SHORTCUT_OVERRIDES_PREF_KEY,
            TOOL_ORDER_PREF_KEY,
        ],
    );
    let initial_pref = startup_preferences.get("theme_mode").cloned();
    let initial_database_search_pref = startup_preferences
        .get(DATABASE_SEARCH_SETTINGS_PREF_KEY)
        .cloned();
    let initial_database_result_pref = startup_preferences
        .get(DATABASE_RESULT_SETTINGS_PREF_KEY)
        .cloned();
    let initial_redis_tree_pref = startup_preferences
        .get(REDIS_TREE_SETTINGS_PREF_KEY)
        .cloned();
    let initial_system_settings_pref = startup_preferences.get(SYSTEM_SETTINGS_PREF_KEY).cloned();
    let initial_shortcut_overrides = startup_preferences
        .get(ramag_ui::shortcuts_dialog::SHORTCUT_OVERRIDES_PREF_KEY)
        .cloned();

    // 启动时同步读取剪贴板开关，避免恢复到已隐藏的工具。
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
                    warn!(
                        operation = "clipboard_settings_load",
                        error = %error,
                        fallback = "tool_hidden",
                        "load clipboard settings failed"
                    );
                    false
                }
            }
        });
        registry.set_enabled(ClipboardTool::ID, clipboard_enabled);
    }
    if let Some(saved_order) = startup_preferences.get(TOOL_ORDER_PREF_KEY)
        && let Err(error) = registry.apply_order_json(saved_order)
    {
        warn!(
            operation = "tool_order_load",
            error = %error,
            "ignore invalid saved tool layout"
        );
    }
    info!(
        operation = "tool_registry_init",
        tool_count = registry.count(),
        "tools registered"
    );

    let deps = AppDeps {
        registry,
        conn_service,
        redis_service,
        mongo_service,
        kafka_service,
        data_sync_service,
        data_sync_gate,
        clipboard_service,
        ssh_service,
        object_storage_service,
        update_service,
        storage,
    };

    let app = gpui_platform::application().with_assets(RamagAssets);

    // 必须在 app.run 前注册；仅在无窗口时重新打开主窗口。
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
            warn!(operation = "database_search_settings_load", error, "ignore invalid database search settings");
        }
        if let Err(error) = init_redis_tree_settings(initial_redis_tree_pref.as_deref(), cx) {
            warn!(operation = "redis_tree_settings_load", error, "ignore invalid Redis tree settings");
        }
        if let Err(error) = init_system_settings(initial_system_settings_pref.as_deref(), cx) {
            warn!(operation = "system_settings_load", error, "ignore invalid system settings");
        }
        if let Err(error) =
            init_database_result_settings(initial_database_result_pref.as_deref(), cx)
        {
            warn!(operation = "database_result_settings_load", error, "ignore invalid database result settings");
        }
        cx.set_global(StorageGlobal(deps.storage.clone()));
        cx.activate(true);

        let data_sync_gate_for_quit = deps.data_sync_gate.clone();
        cx.on_action(move |_: &Quit, cx| {
            if !data_sync_gate_for_quit.is_blocking() {
                cx.quit();
            }
        });

        // 退出时关闭运行时，避免后台任务残留。
        let ssh_service_for_quit = deps.ssh_service.clone();
        let object_storage_for_quit = deps.object_storage_service.clone();
        cx.on_app_quit(move |_| {
            let ssh_service = ssh_service_for_quit.clone();
            let object_storage = object_storage_for_quit.clone();
            async move {
                if let Err(error) = ssh_service.shutdown().await {
                    warn!(operation = "ssh_shutdown", error = %error, "shutdown ssh tool resources failed");
                }
                if let Err(error) = object_storage.shutdown().await {
                    warn!(operation = "object_storage_shutdown", error = %error, "shutdown object storage resources failed");
                }
                ramag_infra_tunnel::shutdown_all();
            }
        })
        .detach();

        // Windows 仅在系统设置开启且托盘安装成功时维持后台运行。
        // macOS 依靠 dock 的 on_reopen，无需此回调。
        #[cfg(target_os = "windows")]
        {
            let tray = std::rc::Rc::new(std::cell::RefCell::new(tray::TrayIcon::install()));
            let tray_resident = tray.borrow().is_some();
            cx.set_global(TrayResident(tray_resident));
            if tray_resident {
                spawn_tray_loop(tray.clone(), deps.clone(), cx);
            } else {
                warn!(operation = "tray_install", reason = "unavailable", "tray unavailable; app quits when the last window closes");
            }
            // 退出前移除托盘图标，避免任务栏残影。
            cx.on_app_quit(move |_| {
                let tray = tray.borrow_mut().take();
                async move {
                    drop(tray);
                }
            })
            .detach();
            cx.on_window_closed(move |cx, _window_id| {
                if cx.windows().is_empty()
                    && !should_keep_running_in_tray(
                        tray_resident,
                        ramag_ui::system_settings(cx),
                    )
                {
                    cx.quit();
                }
            })
            .detach();
            // 次实例通过命名事件请求唤起。
            spawn_instance_activation(instance_guard, deps.clone(), cx);
        }

        // Linux 关闭最后窗口即退出；重复启动会唤起已有实例。
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

        // 视图未消费 Cmd/Ctrl+W 时，才关闭非主窗口。
        // 必须延后关闭：按键分发期间窗口已被取出，直接更新会重入失败。
        cx.on_action(|_: &CloseTab, cx: &mut App| {
            let Some(handle) = cx
                .active_window()
                .or_else(|| cx.windows().into_iter().next())
            else {
                return;
            };
            // 主窗口保留；无标签可关闭时不退出，避免丢失会话和查询稿。
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
            KeyBinding::new("secondary-p", OpenRecentItems, None),
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
            KeyBinding::new("secondary-enter", CommitNow, Some("VcsView")),
            KeyBinding::new("secondary-shift-k", PushNow, Some("VcsView")),
            KeyBinding::new("secondary-t", PullNow, Some("VcsView")),
            KeyBinding::new("secondary-shift-h", ToggleHistoryPane, Some("VcsView")),
            KeyBinding::new("secondary-t", NewSshTerminal, Some("SshWorkspace")),
            KeyBinding::new("secondary-w", CloseSshTerminal, Some("SshWorkspace")),
            KeyBinding::new("secondary-w", CloseSshTerminal, Some("Terminal")),
        ]);

        ramag_ui::shortcuts_dialog::init_shortcut_overrides(
            initial_shortcut_overrides.as_deref(),
            cx,
        );
        ramag_ui::shortcuts_dialog::apply_saved_shortcut_overrides(cx);

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        cx.bind_keys([
            KeyBinding::new("down", SelectNextClip, Some("ClipboardView")),
            KeyBinding::new("up", SelectPrevClip, Some("ClipboardView")),
        ]);

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(svc) = deps.clipboard_service.clone() {
            let cleanup_service = svc.clone();
            cx.spawn(async move |_| {
                if let Err(e) = cleanup_service.cleanup_orphans().await {
                    tracing::warn!(operation = "clipboard_media_orphan_cleanup", error = %e, "clipboard orphan cleanup failed");
                }
            })
            .detach();
            let preload_service = svc.clone();
            cx.spawn(async move |_| preload_service.preload().await)
                .detach();
            spawn_clipboard_capture(svc.clone(), cx);
            spawn_clipboard_hotkey(svc, deps.registry.clone(), deps.clone(), cx);
        }

        let log_path_for_open = log_path.clone();
        cx.on_action(move |_: &OpenLogDir, cx: &mut App| {
            let Some(dir) = log_path_for_open
                .as_ref()
                .and_then(|path| path.parent())
                .map(std::path::Path::to_path_buf)
            else {
                warn!(operation = "logging_file_open", reason = "unavailable", "log file unavailable");
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
                    warn!(operation = "logging_directory_open", error = %error, "open log directory failed");
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
            spawn_update_checks(service, cx);
        }
    });
    info!(operation = "application_stop", "application stopped");
}

const INITIAL_UPDATE_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

fn spawn_update_checks(service: Arc<UpdateService>, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(INITIAL_UPDATE_CHECK_DELAY)
            .await;
        loop {
            // 每次检查都访问远端，避免缓存延迟新版本提示。
            let result = service.check(true).await;
            cx.update(|cx| match result {
                Ok(result) => sync_update_indicator(&result, cx),
                Err(error) => {
                    warn!(operation = "application_update_check", error = %error, "automatic update check failed");
                }
            });
            cx.background_executor().timer(AUTO_CHECK_INTERVAL).await;
        }
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
#[path = "main_tests.rs"]
mod tests;
