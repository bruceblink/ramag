//! 剪贴板采集、全局热键与抽屉窗口生命周期。

use super::*;

/// 轮询剪贴板变化的间隔。
pub(super) const CAPTURE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);
/// 采集失败的最大退避间隔。
pub(super) const CAPTURE_MAX_RETRY_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10);

pub(super) fn next_capture_retry_interval(current: std::time::Duration) -> std::time::Duration {
    current.saturating_mul(2).min(CAPTURE_MAX_RETRY_INTERVAL)
}

/// 在前台 executor 读取剪贴板，满足 AppKit 约束。
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
                    // 保留旧序列号，以便重试同一内容。
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

/// 注册主窗口和剪贴板抽屉热键。
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
        // 跨抽屉窗口复用，缓存内部受容量限制。
        let image_cache = ClipboardImageCache::new();
        // 防止窗口初次显示时被失焦逻辑关闭。
        let mut was_active = false;
        loop {
            // 定时器仅用于设置同步和失焦检查。
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
                // 先注销旧组合，避免短暂并存。
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
                    // 关闭已打开的抽屉。
                    if let Some(handle) = drawer.take() {
                        let _ = cx
                            .update(|cx| handle.update(cx, |_, window, _| window.remove_window()));
                        was_active = false;
                    }
                }
            }

            // 已激活后失焦则隐藏。
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
                    // 立即丢弃失效窗口句柄。
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
                        // 记录前台应用后打开抽屉。
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

    // PopUp 支持全屏空间；激活应用以支持输入法。
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
            // Windows 需窗口级激活；macOS 同样受益。
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
