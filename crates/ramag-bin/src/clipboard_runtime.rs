//! 剪贴板采集、全局热键与抽屉窗口生命周期。

use super::*;

/// 采集间隔。两平台统一轮询系统剪贴板序列号，仅在变化时读取内容。
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
                        operation = "clipboard_capture_loop",
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

/// 主窗口唤醒热键始终注册；剪贴板抽屉热键随剪贴板总开关启停。
pub(super) fn spawn_clipboard_hotkey(
    service: Arc<ClipboardService>,
    registry: Arc<ToolRegistry>,
    deps: AppDeps,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let mut enabled = service.prime_capture_enabled().await;
        cx.update(|cx| sync_clipboard_tool_visibility(&registry, enabled, cx));
        let mut alternate = service.alternate_hotkey();
        let mut listener = HotkeyListener::register_clipboard_hotkey(alternate, enabled);
        if enabled {
            if !listener
                .as_ref()
                .is_some_and(HotkeyListener::clipboard_registered)
            {
                error!(
                    operation = "clipboard_hotkey_register",
                    impact = "clipboard_drawer_disabled",
                    alternate,
                    reason = "registration_rejected",
                    "global hotkey registration failed"
                );
            }
            service.set_hotkey_state(
                if listener
                    .as_ref()
                    .is_some_and(HotkeyListener::clipboard_registered)
                {
                    ramag_app::HotkeyState::Registered
                } else {
                    ramag_app::HotkeyState::Failed
                },
            );
        } else {
            service.set_hotkey_state(ramag_app::HotkeyState::Disabled);
        }

        let mut drawer: Option<gpui::AnyWindowHandle> = None;
        // 图片与应用图标跨抽屉窗口复用；缓存内部有条目数和字节数双重上限。
        let image_cache = ClipboardImageCache::new();
        // 抽屉是否曾真正激活过：避免刚打开（尚未激活）就被失焦逻辑误关
        let mut was_active = false;
        loop {
            // 热键走事件唤醒；短定时器仅承担设置同步与失焦关窗，不再增加唤醒延迟。
            let (pending_event, listener_closed) = if let Some(listener) = listener.as_ref() {
                let receive = Box::pin(listener.recv());
                let housekeeping = Box::pin(cx.background_executor().timer(HOTKEY_POLL_INTERVAL));
                match futures::future::select(receive, housekeeping).await {
                    futures::future::Either::Left((Some(event), _)) => (Some(event), false),
                    futures::future::Either::Left((None, _)) => (None, true),
                    futures::future::Either::Right(_) => (None, false),
                }
            } else {
                cx.background_executor().timer(HOTKEY_POLL_INTERVAL).await;
                (None, false)
            };

            if listener_closed {
                warn!(
                    operation = "global_hotkey_receive",
                    "global hotkey event channel closed"
                );
                drop(listener.take());
                if enabled {
                    service.set_hotkey_state(ramag_app::HotkeyState::Failed);
                }
            }

            let now_enabled = service.capture_enabled();
            let now_alternate = service.alternate_hotkey();
            if now_enabled != enabled || now_alternate != alternate {
                enabled = now_enabled;
                alternate = now_alternate;
                cx.update(|cx| sync_clipboard_tool_visibility(&registry, enabled, cx));
                // 先注销旧组合，避免切换时新旧热键短暂并存。
                drop(listener.take());
                listener = HotkeyListener::register_clipboard_hotkey(alternate, enabled);
                if enabled {
                    if !listener
                        .as_ref()
                        .is_some_and(HotkeyListener::clipboard_registered)
                    {
                        error!(
                            operation = "clipboard_hotkey_register",
                            stage = "re_register",
                            alternate,
                            reason = "registration_rejected",
                            "global hotkey re-registration failed"
                        );
                    }
                    service.set_hotkey_state(
                        if listener
                            .as_ref()
                            .is_some_and(HotkeyListener::clipboard_registered)
                        {
                            ramag_app::HotkeyState::Registered
                        } else {
                            ramag_app::HotkeyState::Failed
                        },
                    );
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

            let mut events = pending_event.into_iter().collect::<Vec<_>>();
            if let Some(listener) = &listener {
                while let Some(event) = listener.poll() {
                    events.push(event);
                }
            }
            for event in events {
                match event {
                    HotkeyEvent::WakeMainWindow => {
                        if let Some(handle) = drawer.take() {
                            let _ = cx.update(|cx| {
                                handle.update(cx, |_, window, _| window.remove_window())
                            });
                        }
                        was_active = false;
                        cx.update(|cx| reveal_main_window(&deps, cx));
                    }
                    HotkeyEvent::ClipboardDrawer if enabled => {
                        if let Some(handle) = drawer.take() {
                            let _ = cx.update(|cx| {
                                handle.update(cx, |_, window, _| window.remove_window())
                            });
                            was_active = false;
                            continue;
                        }
                        // 未打开 → 唤起：记录前台应用后开抽屉
                        let svc = service.clone();
                        let cache = image_cache.clone();
                        drawer = cx.update(|cx| open_drawer_window(svc, cache, cx));
                        was_active = false;
                    }
                    HotkeyEvent::ClipboardDrawer => {}
                }
            }
        }
    })
    .detach();
}

/// 在前台应用所在显示器底部打开抽屉。
pub(super) fn open_drawer_window(
    service: Arc<ClipboardService>,
    image_cache: ClipboardImageCache,
    cx: &mut App,
) -> Option<gpui::AnyWindowHandle> {
    let started = std::time::Instant::now();
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
            let drawer = create_clipboard_drawer_with_cache(
                service,
                activation_target,
                image_cache,
                window,
                cx,
            );
            let root = cx.new(|cx| Root::new(drawer, window, cx));
            // Windows：cx.activate(true) 是 no-op，须窗口级 activate_window（内部 SetForegroundWindow）
            // 抽屉才能抢到前台，搜索框中文输入法 / 粘贴才正常；macOS 同样受益
            window.activate_window();
            root
        },
    );
    cx.activate(true);
    match result {
        Ok(handle) => {
            tracing::debug!(
                operation = "clipboard_drawer_open",
                elapsed_ms = started.elapsed().as_millis(),
                "clipboard drawer opened"
            );
            Some(handle.into())
        }
        Err(e) => {
            error!(operation = "clipboard_drawer_open", error = %e, "open clipboard drawer failed");
            None
        }
    }
}
