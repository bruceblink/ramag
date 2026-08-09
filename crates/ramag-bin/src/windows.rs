//! 主窗口、托盘和单实例激活生命周期。

use super::*;

pub(super) fn open_main_window(deps: AppDeps, cx: &mut App) {
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
            // 校验保存的位置是否仍落在某个显示器内：外接屏拔除后旧坐标可能整体在屏幕外，
            // 恢复出去将无法拖回。要求标题栏区域（顶部中点）落在某显示器内，保证可拖动，
            // 否则丢弃位置、回退居中最大化
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
                // 原生标题栏需 appears_transparent=false，否则失去双击 zoom 命中区
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
                window.on_window_should_close(cx, move |_, _| !gate_for_close.is_blocking());
                let home_view = cx.new(|_| HomeView::new(registry.clone()));

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
