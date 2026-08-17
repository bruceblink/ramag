//! Windows 系统托盘。
//! 隐藏窗口接收回调，主线程轮询原子事件位图。

use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::JoinHandle;

use tracing::{info, warn};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, MAX_PATH, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::UI::Shell::{
    ExtractIconExW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW, HICON,
    IDI_APPLICATION, LoadIconW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_CLOSE, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NCCREATE, WM_NULL, WM_RBUTTONUP,
    WNDCLASSW,
};
use windows::core::{PCWSTR, w};

/// 托盘回调消息。
const TRAY_CALLBACK: u32 = WM_APP + 1;
const TRAY_ICON_ID: u32 = 1;
const MENU_CMD_OPEN: u32 = 1;
const MENU_CMD_QUIT: u32 = 2;
const EVENT_OPEN: u8 = 1;
const EVENT_QUIT: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayEvent {
    /// 唤起主窗口。
    Open,
    /// 退出应用。
    Quit,
}

/// 托盘句柄，释放时关闭隐藏窗口并回收线程。
pub(crate) struct TrayIcon {
    events: Arc<AtomicU8>,
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl TrayIcon {
    /// 安装托盘图标；失败返回 `None`。
    pub(crate) fn install() -> Option<Self> {
        let events = Arc::new(AtomicU8::new(0));
        let thread_events = events.clone();
        let (ready_tx, ready_rx) = sync_channel::<Option<isize>>(1);
        let thread = match std::thread::Builder::new()
            .name("ramag-tray".into())
            .spawn(move || tray_thread(thread_events, ready_tx))
        {
            Ok(thread) => thread,
            Err(error) => {
                warn!(operation = "tray_install", error = %error, "start tray thread failed");
                return None;
            }
        };
        match ready_rx.recv() {
            Ok(Some(hwnd)) => {
                info!(operation = "tray_install", "tray icon installed");
                Some(Self {
                    events,
                    hwnd,
                    thread: Some(thread),
                })
            }
            Ok(None) => {
                join_thread(thread);
                None
            }
            Err(error) => {
                warn!(operation = "tray_install", stage = "initialization_channel", error = %error, "tray initialization channel closed");
                join_thread(thread);
                None
            }
        }
    }

    /// 非阻塞读取一个托盘事件。
    pub(crate) fn poll(&self) -> Option<TrayEvent> {
        let events = self.events.swap(0, Ordering::AcqRel);
        if events & EVENT_QUIT != 0 {
            Some(TrayEvent::Quit)
        } else if events & EVENT_OPEN != 0 {
            Some(TrayEvent::Open)
        } else {
            None
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let posted = unsafe { PostMessageW(HWND(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            match posted {
                Ok(()) => join_thread(thread),
                Err(error) if thread.is_finished() => {
                    warn!(operation = "tray_shutdown", stage = "post_exit_signal", error = %error, "post tray shutdown failed after thread exit");
                    join_thread(thread);
                }
                Err(error) => {
                    // 无法唤醒时不能阻塞 join。
                    warn!(operation = "tray_shutdown", stage = "detach_signal", error = %error, "post tray shutdown failed; detaching thread");
                }
            }
        }
        info!(operation = "tray_shutdown", "tray icon removed");
    }
}

fn join_thread(thread: JoinHandle<()>) {
    if thread.join().is_err() {
        warn!(
            operation = "tray_shutdown",
            stage = "thread_join",
            "tray thread panicked"
        );
    }
}

/// 托盘线程：创建隐藏窗口、挂图标并运行消息泵。
fn tray_thread(events: Arc<AtomicU8>, ready_tx: SyncSender<Option<isize>>) {
    let hwnd = match create_tray_window(events) {
        Ok(hwnd) => hwnd,
        Err(reason) => {
            warn!(
                operation = "tray_install",
                stage = "window_create",
                reason,
                "create tray window failed"
            );
            let _ = ready_tx.send(None);
            return;
        }
    };
    if !add_tray_icon(hwnd) {
        // 通过 WM_CLOSE 销毁窗口。
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        pump_messages();
        let _ = ready_tx.send(None);
        return;
    }
    if ready_tx.send(Some(hwnd.0)).is_err() {
        warn!(
            operation = "tray_install",
            stage = "initialization_receiver",
            "tray initialization receiver dropped"
        );
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        pump_messages();
        return;
    }
    pump_messages();
}

fn pump_messages() {
    let mut msg = MSG::default();
    // 0 为 WM_QUIT，负值为错误。
    loop {
        let status = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if status <= 0 {
            if status < 0 {
                let error = windows::core::Error::from_win32();
                warn!(operation = "tray_message_loop", error = %error, "tray GetMessageW failed");
            }
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 注册窗口类并创建隐藏窗口。
fn create_tray_window(events: Arc<AtomicU8>) -> Result<HWND, &'static str> {
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|_| "GetModuleHandleW failed")?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(tray_wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("RamagTrayWindow"),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let events_ptr = Box::into_raw(Box::new(events));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("RamagTrayWindow"),
            w!("Ramag Tray"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            Some(events_ptr.cast()),
        );
        if hwnd.0 == 0 {
            drop(Box::from_raw(events_ptr));
            return Err("CreateWindowExW failed");
        }
        Ok(hwnd)
    }
}

fn add_tray_icon(hwnd: HWND) -> bool {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK,
        hIcon: load_app_icon(),
        ..Default::default()
    };
    let tip: Vec<u16> = "Ramag".encode_utf16().collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
    if !added {
        warn!(operation = "tray_icon_add", "add system tray icon failed");
    }
    added
}

/// 加载应用图标，失败回退系统图标。
fn load_app_icon() -> HICON {
    unsafe {
        let mut path = [0u16; MAX_PATH as usize];
        let len = GetModuleFileNameW(None, &mut path) as usize;
        if len > 0 && len < path.len() {
            let mut small = HICON::default();
            if ExtractIconExW(PCWSTR(path.as_ptr()), 0, None, Some(&mut small), 1) > 0
                && !small.is_invalid()
            {
                return small;
            }
        }
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}

/// 显示右键菜单并返回选择。
fn show_tray_menu(hwnd: HWND) -> Option<u32> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let result = (|| -> Option<u32> {
            AppendMenuW(menu, MF_STRING, MENU_CMD_OPEN as usize, w!("打开 Ramag")).ok()?;
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok()?;
            AppendMenuW(menu, MF_STRING, MENU_CMD_QUIT as usize, w!("退出")).ok()?;
            let mut point = POINT::default();
            GetCursorPos(&mut point).ok()?;
            // 前台化后，菜单才能在点击外部时正常关闭。
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                point.x,
                point.y,
                0,
                hwnd,
                None,
            );
            let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            (cmd.0 != 0).then_some(cmd.0 as u32)
        })();
        if let Err(error) = DestroyMenu(menu) {
            warn!(operation = "tray_shutdown", stage = "menu_destroy", error = %error, "destroy tray menu failed");
        }
        result
    }
}

fn send_event(hwnd: HWND, event: TrayEvent) {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Arc<AtomicU8>;
    if let Some(events) = unsafe { ptr.as_ref() } {
        let bit = match event {
            TrayEvent::Open => EVENT_OPEN,
            TrayEvent::Quit => EVENT_QUIT,
        };
        events.fetch_or(bit, Ordering::Release);
    }
}

unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // 保存事件位图，供回调使用。
            let create = lparam.0 as *const CREATESTRUCTW;
            if let Some(create) = unsafe { create.as_ref() } {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        TRAY_CALLBACK => {
            match lparam.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => send_event(hwnd, TrayEvent::Open),
                WM_RBUTTONUP => match show_tray_menu(hwnd) {
                    Some(MENU_CMD_OPEN) => send_event(hwnd, TrayEvent::Open),
                    Some(MENU_CMD_QUIT) => send_event(hwnd, TrayEvent::Quit),
                    _ => {}
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ICON_ID,
                ..Default::default()
            };
            unsafe {
                // 删除未挂载图标也无害。
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                // 回收事件位图，阻止后续发送。
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut Arc<AtomicU8>;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                }
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
