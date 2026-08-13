//! 全局热键：Win32 注册剪贴板抽屉与 Ramag 主窗口唤醒组合键。
//! RegisterHotKey 的 WM_HOTKEY 投递到注册线程的消息队列，故需一条专属线程跑消息泵，
//! 事件经 mpsc channel 转出，由 main.rs 计时器轮询消费（与 macOS 侧同款模式）。

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use tracing::{info, warn};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT, WM_USER,
};

const CLIPBOARD_HOTKEY_ID: i32 = 1;
const WAKE_MAIN_WINDOW_HOTKEY_ID: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    ClipboardDrawer,
    WakeMainWindow,
}

/// 热键监听句柄：持有消息线程 id（Drop 时投 WM_QUIT 令其退出）与事件 Receiver
pub struct HotkeyListener {
    rx: Receiver<HotkeyEvent>,
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
    clipboard_registered: bool,
}

impl HotkeyListener {
    /// 固定注册主窗口唤醒键；剪贴板启用时再注册抽屉键。
    pub fn register_clipboard_hotkey(alternate: bool, clipboard_enabled: bool) -> Option<Self> {
        let combo = if alternate {
            "ctrl-alt-v"
        } else {
            "ctrl-shift-v"
        };
        let (tx, rx) = sync_channel::<HotkeyEvent>(4);
        // 用于把线程 id / 注册结果回传主线程
        let (ready_tx, ready_rx) = sync_channel::<Option<(u32, bool)>>(1);

        let handle = match std::thread::Builder::new()
            .name("ramag-hotkey".into())
            .spawn(move || hotkey_thread(alternate, clipboard_enabled, tx, ready_tx))
        {
            Ok(handle) => handle,
            Err(error) => {
                warn!(operation = "clipboard_hotkey_thread_start", error = %error, "start hotkey thread failed");
                return None;
            }
        };

        match ready_rx.recv() {
            Ok(Some((thread_id, clipboard_registered))) => {
                info!(
                    operation = "global_hotkey_register",
                    combo, clipboard_enabled, "global hotkeys registered"
                );
                Some(Self {
                    rx,
                    thread_id,
                    handle: Some(handle),
                    clipboard_registered,
                })
            }
            Ok(None) => {
                join_thread(handle);
                None
            }
            Err(error) => {
                warn!(operation = "clipboard_hotkey_register", stage = "initialization_channel", error = %error, "hotkey initialization channel closed");
                join_thread(handle);
                None
            }
        }
    }

    pub fn clipboard_registered(&self) -> bool {
        self.clipboard_registered
    }

    pub fn poll(&self) -> Option<HotkeyEvent> {
        self.rx.try_recv().ok()
    }
}

/// 热键线程：注册 → 回传结果 → 消息泵；收到 WM_HOTKEY 转发信号，收到 WM_QUIT 退出并注销
fn hotkey_thread(
    alternate: bool,
    clipboard_enabled: bool,
    tx: SyncSender<HotkeyEvent>,
    ready_tx: SyncSender<Option<(u32, bool)>>,
) {
    unsafe {
        let thread_id = GetCurrentThreadId();
        // 显式创建消息队列，确保主线程收到 ready 后可可靠投递 WM_QUIT。
        let mut queue_probe = MSG::default();
        let _ = PeekMessageW(&mut queue_probe, None, WM_USER, WM_USER, PM_NOREMOVE);
        let second = if alternate { MOD_ALT } else { MOD_SHIFT };
        let modifiers = MOD_CONTROL | second | MOD_NOREPEAT;
        let clipboard_registered = if clipboard_enabled {
            match RegisterHotKey(None, CLIPBOARD_HOTKEY_ID, modifiers, VK_V.0 as u32) {
                Ok(()) => true,
                Err(error) => {
                    warn!(operation = "clipboard_hotkey_register", error = %error, "register clipboard hotkey failed");
                    false
                }
            }
        } else {
            false
        };
        let wake_registered = match RegisterHotKey(
            None,
            WAKE_MAIN_WINDOW_HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT,
            VK_V.0 as u32,
        ) {
            Ok(()) => true,
            Err(error) => {
                warn!(operation = "main_window_hotkey_register", error = %error, "register main window hotkey failed");
                false
            }
        };
        if !clipboard_registered && !wake_registered {
            let _ = ready_tx.send(None);
            return;
        }
        if ready_tx
            .send(Some((thread_id, clipboard_registered)))
            .is_err()
        {
            warn!(
                operation = "global_hotkey_register",
                stage = "receiver_dropped",
                "hotkey initialization receiver dropped"
            );
            if clipboard_registered {
                let _ = UnregisterHotKey(None, CLIPBOARD_HOTKEY_ID);
            }
            if wake_registered {
                let _ = UnregisterHotKey(None, WAKE_MAIN_WINDOW_HOTKEY_ID);
            }
            return;
        }

        let mut msg = MSG::default();
        // GetMessageW 返回 >0 正常、0 收到 WM_QUIT、-1 出错，后两者均退出循环
        loop {
            let status = GetMessageW(&mut msg, None, 0, 0).0;
            if status <= 0 {
                if status < 0 {
                    let error = windows::core::Error::from_win32();
                    warn!(operation = "clipboard_hotkey_poll", error = %error, "read clipboard hotkey message failed");
                }
                break;
            }
            if msg.message == WM_HOTKEY {
                let event = match msg.wParam.0 as i32 {
                    CLIPBOARD_HOTKEY_ID => Some(HotkeyEvent::ClipboardDrawer),
                    WAKE_MAIN_WINDOW_HOTKEY_ID => Some(HotkeyEvent::WakeMainWindow),
                    _ => None,
                };
                if let Some(event) = event {
                    match tx.try_send(event) {
                        Ok(()) | Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
            }
        }
        if clipboard_registered && let Err(error) = UnregisterHotKey(None, CLIPBOARD_HOTKEY_ID) {
            warn!(operation = "clipboard_hotkey_unregister", error = %error, "unregister clipboard hotkey failed");
        }
        if wake_registered && let Err(error) = UnregisterHotKey(None, WAKE_MAIN_WINDOW_HOTKEY_ID) {
            warn!(operation = "main_window_hotkey_unregister", error = %error, "unregister main window hotkey failed");
        }
    }
}

fn join_thread(handle: JoinHandle<()>) {
    if handle.join().is_err() {
        warn!(
            operation = "clipboard_hotkey_thread_shutdown",
            "hotkey thread panicked"
        );
    }
}

impl Drop for HotkeyListener {
    /// 向消息线程投 WM_QUIT 令其跳出消息泵并注销热键，再 join 回收
    fn drop(&mut self) {
        let posted = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        let mut stopped = false;
        if let Some(handle) = self.handle.take() {
            match posted {
                Ok(()) => {
                    join_thread(handle);
                    stopped = true;
                }
                Err(error) if handle.is_finished() => {
                    warn!(operation = "clipboard_hotkey_thread_shutdown", stage = "post_exit_signal", error = %error, "post hotkey shutdown message failed after thread exit");
                    join_thread(handle);
                    stopped = true;
                }
                Err(error) => {
                    // 无法唤醒时不能阻塞 join；进程退出会回收已分离的线程。
                    warn!(operation = "clipboard_hotkey_thread_shutdown", stage = "detach_signal", error = %error, "post hotkey shutdown message failed; detaching thread");
                }
            }
        }
        if stopped {
            info!(
                operation = "global_hotkey_unregister",
                "global hotkeys unregistered"
            );
        }
    }
}
