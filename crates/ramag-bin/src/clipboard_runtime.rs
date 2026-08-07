//! 剪贴板采集、全局热键与抽屉窗口生命周期。

use super::*;

pub(super) const CAPTURE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);
/// 持久化或系统剪贴板持续失败时指数退避，避免每 400ms 重复占用 CPU 与刷日志。
pub(super) const CAPTURE_MAX_RETRY_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10);

pub(super) fn next_capture_retry_interval(current: std::time::Duration) -> std::time::Duration {
    current.saturating_mul(2).min(CAPTURE_MAX_RETRY_INTERVAL)
}

/// 驱动读取留在前台 executor，满足 macOS AppKit 主线程约束。
pub(super) fn spawn_clipboard_capture(service: Arc<ClipboardService>, cx: &mut App) {
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

pub(super) const HOTKEY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

pub(super) fn sync_clipboard_tool_visibility(
    registry: &Arc<ToolRegistry>,
    enabled: bool,
    cx: &mut App,
) {
    if registry.set_enabled(ClipboardTool::ID, enabled) {
        cx.refresh_windows();
    }
}

/// 热键随剪贴板总开关注册或释放；注册失败不影响其他功能。
pub(super) fn spawn_clipboard_hotkey(
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
                error!(
                    impact = "clipboard_drawer_disabled",
                    "global hotkey registration failed"
                );
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
                        error!("global hotkey re-registration failed");
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

/// 在前台应用所在显示器底部打开抽屉。
pub(super) fn open_drawer_window(
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
            error!(error = %e, "open clipboard drawer failed");
            None
        }
    }
}
