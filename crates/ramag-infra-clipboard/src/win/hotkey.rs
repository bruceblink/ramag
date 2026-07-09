//! 全局热键：Win32 RegisterHotKey 注册 Ctrl-Shift-V（对应 macOS 的 cmd-shift-V）。
//! RegisterHotKey 的 WM_HOTKEY 投递到注册线程的消息队列，故需一条专属线程跑消息泵，
//! 事件经 mpsc channel 转出，由 main.rs 计时器轮询消费（与 macOS 侧同款模式）。

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use tracing::{info, warn};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY, WM_QUIT};

const HOTKEY_ID: i32 = 1;

/// 热键监听句柄：持有消息线程 id（Drop 时投 WM_QUIT 令其退出）与事件 Receiver
pub struct HotkeyListener {
    rx: Receiver<()>,
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

impl HotkeyListener {
    /// 注册 Ctrl-Shift-V。启一条线程注册热键并跑消息泵；注册失败返回 None（不影响其余功能）
    pub fn register_cmd_shift_v() -> Option<Self> {
        let (tx, rx) = channel::<()>();
        // 用于把线程 id / 注册结果回传主线程
        let (ready_tx, ready_rx) = channel::<Option<u32>>();

        let handle = std::thread::spawn(move || hotkey_thread(tx, ready_tx));

        match ready_rx.recv() {
            Ok(Some(thread_id)) => {
                info!("global hotkey ctrl-shift-v registered");
                Some(Self {
                    rx,
                    thread_id,
                    handle: Some(handle),
                })
            }
            _ => {
                warn!("RegisterHotKey failed");
                let _ = handle.join();
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
fn hotkey_thread(tx: Sender<()>, ready_tx: Sender<Option<u32>>) {
    unsafe {
        let thread_id = GetCurrentThreadId();
        let modifiers = MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT;
        if RegisterHotKey(None, HOTKEY_ID, modifiers, VK_V.0 as u32).is_err() {
            let _ = ready_tx.send(None);
            return;
        }
        let _ = ready_tx.send(Some(thread_id));

        let mut msg = MSG::default();
        // GetMessageW 返回 >0 正常、0 收到 WM_QUIT、-1 出错，后两者均退出循环
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_HOTKEY {
                let _ = tx.send(());
            }
        }
        let _ = UnregisterHotKey(None, HOTKEY_ID);
    }
}

impl Drop for HotkeyListener {
    /// 向消息线程投 WM_QUIT 令其跳出消息泵并注销热键，再 join 回收
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        info!("global hotkey ctrl-shift-v unregistered");
    }
}
