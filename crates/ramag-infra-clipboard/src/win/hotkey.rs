//! 全局热键：Win32 RegisterHotKey 注册 Ctrl-Shift-V（备用 Ctrl-Alt-V，对应 macOS 侧组合）。
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

const HOTKEY_ID: i32 = 1;

/// 热键监听句柄：持有消息线程 id（Drop 时投 WM_QUIT 令其退出）与事件 Receiver
pub struct HotkeyListener {
    rx: Receiver<()>,
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
    /// 已注册组合名，注销日志用
    combo: &'static str,
}

impl HotkeyListener {
    /// 注册剪贴板热键（默认 Ctrl-Shift-V，alternate 为 Ctrl-Alt-V）。
    /// 启一条线程注册热键并跑消息泵；注册失败返回 None（不影响其余功能）
    pub fn register_clipboard_hotkey(alternate: bool) -> Option<Self> {
        let combo = if alternate {
            "ctrl-alt-v"
        } else {
            "ctrl-shift-v"
        };
        let (tx, rx) = sync_channel::<()>(1);
        // 用于把线程 id / 注册结果回传主线程
        let (ready_tx, ready_rx) = sync_channel::<Option<u32>>(1);

        let handle = match std::thread::Builder::new()
            .name("ramag-hotkey".into())
            .spawn(move || hotkey_thread(alternate, tx, ready_tx))
        {
            Ok(handle) => handle,
            Err(error) => {
                warn!(error = %error, "start hotkey thread failed");
                return None;
            }
        };

        match ready_rx.recv() {
            Ok(Some(thread_id)) => {
                info!(combo, "global clipboard hotkey registered");
                Some(Self {
                    rx,
                    thread_id,
                    handle: Some(handle),
                    combo,
                })
            }
            Ok(None) => {
                join_thread(handle);
                None
            }
            Err(error) => {
                warn!(error = %error, "hotkey initialization channel closed");
                join_thread(handle);
                None
            }
        }
    }

    /// 非阻塞取一次热键事件（drain 后返回是否发生过）
    pub fn poll(&self) -> bool {
        let mut fired = false;
        while self.rx.try_recv().is_ok() {
            fired = true;
        }
        fired
    }
}

/// 热键线程：注册 → 回传结果 → 消息泵；收到 WM_HOTKEY 转发信号，收到 WM_QUIT 退出并注销
fn hotkey_thread(alternate: bool, tx: SyncSender<()>, ready_tx: SyncSender<Option<u32>>) {
    unsafe {
        let thread_id = GetCurrentThreadId();
        // 显式创建消息队列，确保主线程收到 ready 后可可靠投递 WM_QUIT。
        let mut queue_probe = MSG::default();
        let _ = PeekMessageW(&mut queue_probe, None, WM_USER, WM_USER, PM_NOREMOVE);
        let second = if alternate { MOD_ALT } else { MOD_SHIFT };
        let modifiers = MOD_CONTROL | second | MOD_NOREPEAT;
        if let Err(error) = RegisterHotKey(None, HOTKEY_ID, modifiers, VK_V.0 as u32) {
            warn!(error = %error, "register clipboard hotkey failed");
            if ready_tx.send(None).is_err() {
                warn!("hotkey initialization receiver dropped after registration failure");
            }
            return;
        }
        if ready_tx.send(Some(thread_id)).is_err() {
            warn!("hotkey initialization receiver dropped");
            if let Err(error) = UnregisterHotKey(None, HOTKEY_ID) {
                warn!(error = %error, "unregister cancelled clipboard hotkey failed");
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
                    warn!(error = %error, "read clipboard hotkey message failed");
                }
                break;
            }
            if msg.message == WM_HOTKEY {
                // 容量为 1：已有待处理信号时合并重复按键，避免 UI 忙时无界积压。
                match tx.try_send(()) {
                    Ok(()) | Err(TrySendError::Full(())) => {}
                    Err(TrySendError::Disconnected(())) => break,
                }
            }
        }
        if let Err(error) = UnregisterHotKey(None, HOTKEY_ID) {
            warn!(error = %error, "unregister clipboard hotkey failed");
        }
    }
}

fn join_thread(handle: JoinHandle<()>) {
    if handle.join().is_err() {
        warn!("hotkey thread panicked");
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
                    warn!(error = %error, "post hotkey shutdown message failed after thread exit");
                    join_thread(handle);
                    stopped = true;
                }
                Err(error) => {
                    // 无法唤醒时不能阻塞 join；进程退出会回收已分离的线程。
                    warn!(error = %error, "post hotkey shutdown message failed; detaching thread");
                }
            }
        }
        if stopped {
            info!(combo = self.combo, "global clipboard hotkey unregistered");
        }
    }
}
