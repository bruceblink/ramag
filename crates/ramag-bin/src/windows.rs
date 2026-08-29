//! 主窗口、托盘和单实例激活生命周期。

use super::*;
use ramag_ui::SystemSettings;

/// 主窗口重建时复用的依赖。
#[derive(Clone)]
pub(super) struct AppDeps {
    pub(super) registry: Arc<ToolRegistry>,
    pub(super) conn_service: Arc<ConnectionService>,
    pub(super) redis_service: Arc<RedisService>,
    pub(super) mongo_service: Arc<MongoService>,
    pub(super) data_sync_service: Arc<DataSyncService>,
    pub(super) data_sync_gate: Arc<DataSyncGate>,
    pub(super) clipboard_service: Option<Arc<ClipboardService>>,
    pub(super) ssh_service: Arc<SshService>,
    pub(super) object_storage_service: Arc<ObjectStorageService>,
    pub(super) update_service: Option<Arc<UpdateService>>,
    pub(super) storage: Arc<dyn Storage>,
}

/// 托盘和单实例激活优先复用此窗口。
pub(super) struct MainWindowGlobal(pub(super) gpui::AnyWindowHandle);

impl gpui::Global for MainWindowGlobal {}

/// 记录系统托盘是否已成功安装，关闭最后窗口时据此决定是否允许后台驻留。
pub(super) struct TrayResident(pub(super) bool);

impl gpui::Global for TrayResident {}

/// 只有托盘已安装且用户明确开启设置时，关闭最后窗口才保留后台进程。
pub(super) fn should_keep_running_in_tray(tray_resident: bool, settings: SystemSettings) -> bool {
    tray_resident && settings.minimize_to_tray
}

/// 根据当前设置选择 GPUI 的自动退出模式；托盘驻留必须改为显式退出。
pub(super) fn quit_mode_for_window_close(
    tray_resident: bool,
    settings: SystemSettings,
) -> gpui::QuitMode {
    if should_keep_running_in_tray(tray_resident, settings) {
        gpui::QuitMode::Explicit
    } else {
        gpui::QuitMode::Default
    }
}

/// 合并主窗口句柄写入前的重复唤起。
#[derive(Default)]
pub(super) struct MainWindowOpenGate {
    opening: bool,
}

impl gpui::Global for MainWindowOpenGate {}

impl MainWindowOpenGate {
    pub(super) fn try_begin(&mut self) -> bool {
        if self.opening {
            return false;
        }
        self.opening = true;
        true
    }

    pub(super) fn finish(&mut self) {
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

pub(super) fn reveal_main_window(deps: &AppDeps, cx: &mut App) {
    cx.activate(true);
    if let Some(handle) = cx.try_global::<MainWindowGlobal>().map(|global| global.0) {
        // 关闭到托盘后旧句柄仍保存在全局状态；只有句柄仍属于当前窗口集合时才更新它。
        if cx.windows().contains(&handle)
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
    }
    open_main_window(deps.clone(), cx);
}

pub(super) fn open_main_window(deps: AppDeps, cx: &mut App) {
    if !begin_main_window_open(cx) {
        return;
    }
    let window_preferences = read_preferences(
        &deps.storage,
        &["last_tool", ramag_ui::WindowBoundsPref::PREF_KEY],
    );
    // 恢复上次工具；已移除的工具 ID 会被忽略。
    let last_tool = window_preferences
        .get("last_tool")
        .filter(|tool| !tool.is_empty())
        .cloned();
    // 恢复窗口尺寸；无记录则居中最大化。
    let saved_bounds = window_preferences
        .get(ramag_ui::WindowBoundsPref::PREF_KEY)
        .and_then(|json| match ramag_ui::WindowBoundsPref::parse(json) {
            Ok(pref) => Some(pref),
            Err(error) => {
                warn!(
                    operation = "window_bounds_load",
                    error, "ignore invalid saved window bounds"
                );
                None
            }
        });
    let AppDeps {
        registry,
        conn_service,
        redis_service,
        mongo_service,
        data_sync_service,
        data_sync_gate,
        clipboard_service,
        ssh_service,
        object_storage_service,
        update_service,
        storage,
    } = deps;
    let fallback = Bounds::centered(None, size(px(1200.0), px(780.0)), cx);
    let window_bounds = match &saved_bounds {
        Some(p) => {
            let b = Bounds::new(
                gpui::point(px(p.x), px(p.y)),
                size(px(p.w.max(800.0)), px(p.h.max(500.0))),
            );
            // 显示器移除后回退居中，避免窗口恢复到屏幕外。
            let title_pt = gpui::point(b.origin.x + b.size.width / 2.0, b.origin.y + px(16.0));
            let on_screen = cx.displays().iter().any(|d| d.bounds().contains(&title_pt));
            if !on_screen {
                info!(
                    operation = "window_bounds_load",
                    reason = "off_screen",
                    "saved window position is off-screen; falling back to centered"
                );
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
                app_id: Some("com.ramag.Ramag".into()),
                window_bounds: Some(window_bounds),
                window_min_size: Some(size(px(800.0), px(500.0))),
                // 原生标题栏需保留双击缩放热区。
                titlebar: Some(TitlebarOptions {
                    title: if cfg!(any(target_os = "windows", target_os = "linux")) {
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
                let gate_for_close = data_sync_gate.clone();
                window.on_window_should_close(cx, move |_, cx| {
                    if gate_for_close.is_blocking() {
                        return false;
                    }
                    let tray_resident = cx
                        .try_global::<TrayResident>()
                        .is_some_and(|state| state.0);
                    let settings = ramag_ui::system_settings(cx);
                    // 让最后一个窗口关闭后仍保留事件循环，托盘菜单可以重新打开窗口。
                    cx.set_quit_mode(quit_mode_for_window_close(tray_resident, settings));
                    true
                });
                let home_view = cx.new(|cx| HomeView::new(registry.clone(), cx));

                let dbclient_view = create_dbclient_view(
                    conn_service.clone(),
                    redis_service.clone(),
                    mongo_service.clone(),
                    data_sync_service.clone(),
                    window,
                    cx,
                );

                let git_driver: Arc<dyn GitDriver> = Arc::new(GitDriverImpl::new());
                let vcs_view = create_vcs_view(git_driver, storage.clone(), window, cx);

                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let clipboard_view = clipboard_service
                    .as_ref()
                    .map(|service| create_clipboard_view(service.clone(), window, cx));
                let ssh_view = create_ssh_view(ssh_service.clone(), window, cx);
                let object_storage_view =
                    create_object_storage_view(object_storage_service.clone(), window, cx);
                let system_view = create_system_view(window, cx);
                let settings_view = cx.new(|cx| {
                    SettingsView::new(
                        clipboard_service.clone(),
                        conn_service.clone(),
                        ssh_service.clone(),
                        update_service.clone(),
                        window,
                        cx,
                    )
                });

                let shell = cx.new(|cx| {
                    let mut shell =
                        Shell::new(registry.clone(), data_sync_gate.clone(), window, cx);
                    shell.set_home_view(home_view.clone().into());
                    shell.set_settings_view(settings_view.clone().into());
                    shell.register_tool_view(DbClientTool::ID, dbclient_view.clone().into());
                    shell.register_tool_view(VcsTool::ID, vcs_view.into());
                    #[cfg(any(target_os = "macos", target_os = "windows"))]
                    if let Some(clipboard_view) = clipboard_view.clone() {
                        shell.register_tool_view(ClipboardTool::ID, clipboard_view.into());
                    }
                    shell.register_tool_view(SshTool::ID, ssh_view.into());
                    shell.register_tool_view(ObjectStorageTool::ID, object_storage_view.into());
                    shell.register_tool_view(SystemTool::ID, system_view.into());

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

                // 恢复上次工具；已移除时留在 Home。
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
                error!(operation = "application_window_open", error = %err, "open application window failed");
            }
        }
    })
    .detach();
}

#[cfg(target_os = "windows")]
pub(super) fn spawn_tray_loop(
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
                    if deps.data_sync_gate.is_blocking() {
                        continue;
                    }
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

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(super) fn spawn_instance_activation(
    guard: single_instance::PrimaryGuard,
    deps: AppDeps,
    cx: &mut App,
) {
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

#[cfg(test)]
mod tests {
    use super::{quit_mode_for_window_close, should_keep_running_in_tray};
    use ramag_ui::SystemSettings;

    #[test]
    fn tray_residency_requires_installation_and_opt_in() {
        assert!(!should_keep_running_in_tray(
            false,
            SystemSettings::default()
        ));
        assert!(!should_keep_running_in_tray(
            true,
            SystemSettings::default()
        ));
        assert!(should_keep_running_in_tray(
            true,
            SystemSettings {
                minimize_to_tray: true,
            }
        ));
    }

    #[test]
    fn window_close_uses_explicit_quit_only_for_tray_residency() {
        assert_eq!(
            quit_mode_for_window_close(
                true,
                SystemSettings {
                    minimize_to_tray: true,
                }
            ),
            gpui::QuitMode::Explicit
        );
        assert_eq!(
            quit_mode_for_window_close(
                false,
                SystemSettings {
                    minimize_to_tray: true,
                }
            ),
            gpui::QuitMode::Default
        );
        assert_eq!(
            quit_mode_for_window_close(true, SystemSettings::default()),
            gpui::QuitMode::Default
        );
    }
}
