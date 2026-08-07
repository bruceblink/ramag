//! 全局热键：Carbon `RegisterEventHotKey` 注册系统级快捷键（cmd-shift-V / 备用 cmd-alt-V）。
//! 事件回调在主线程触发，经 mpsc channel 转出，由 main.rs 的计时器轮询消费——
//! 与采集循环同款模式，不引入第三方 global-hotkey 依赖

use std::ffi::c_void;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use tracing::{info, warn};

// Carbon FFI 类型。
type OsStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type EventHandlerProc = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerProc,
        num_types: u32,
        type_list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OsStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OsStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
    fn RemoveEventHandler(handler: EventHandlerRef) -> OsStatus;
}

// Carbon 常量：kEventClassKeyboard = 'keyb'，kEventHotKeyPressed = 5。
const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_HOTKEY_PRESSED: u32 = 5;
// Carbon 修饰键掩码
const CMD_KEY: u32 = 0x0100;
const SHIFT_KEY: u32 = 0x0200;
const OPTION_KEY: u32 = 0x0800;
// V 键码为 9。
const KEY_V: u32 = 9;

/// 热键事件回调：经 user_data 还原 Sender 并发信号。回调全程不 panic（跨 FFI 边界）
extern "C" fn hotkey_handler(
    _next: EventHandlerCallRef,
    _event: EventRef,
    user_data: *mut c_void,
) -> OsStatus {
    if !user_data.is_null() {
        // user_data 指向 SyncSender 裸指针，仅借用不接管（注销时由 Drop 回收）
        let tx = unsafe { &*(user_data as *const SyncSender<()>) };
        // 容量为 1：已有待处理信号时合并重复按键，避免主线程繁忙期间无界积压。
        match tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
        }
    }
    0
}

/// 热键句柄：持有 Receiver 与 Carbon ref；Drop 时注销热键、移除 handler、回收 Sender。
/// ref 以 usize 存（裸指针非 Send，须能随句柄移入异步轮询任务）
pub struct HotkeyListener {
    rx: Receiver<()>,
    handler_ref: usize,
    hotkey_ref: usize,
    tx_ptr: usize,
    /// 已注册组合名，注销日志用
    combo: &'static str,
}

impl HotkeyListener {
    /// 注册剪贴板热键（默认 cmd-shift-V，alternate 为 cmd-alt-V）。
    /// 须在主线程、NSApplication 事件循环就绪后调用
    pub fn register_clipboard_hotkey(alternate: bool) -> Option<Self> {
        let (modifiers, combo) = if alternate {
            (CMD_KEY | OPTION_KEY, "cmd-alt-v")
        } else {
            (CMD_KEY | SHIFT_KEY, "cmd-shift-v")
        };
        let (tx, rx) = sync_channel::<()>(1);
        // Sender 转裸指针交给 Carbon 回调；句柄存活期间常驻，注销时由 Drop 回收
        let tx_ptr = Box::into_raw(Box::new(tx)) as *mut c_void;

        unsafe {
            let target = GetApplicationEventTarget();
            let spec = EventTypeSpec {
                event_class: EVENT_CLASS_KEYBOARD,
                event_kind: EVENT_HOTKEY_PRESSED,
            };
            let mut handler_ref: EventHandlerRef = std::ptr::null_mut();
            let status =
                InstallEventHandler(target, hotkey_handler, 1, &spec, tx_ptr, &mut handler_ref);
            if status != 0 {
                warn!(status, "install clipboard hotkey event handler failed");
                drop(Box::from_raw(tx_ptr as *mut SyncSender<()>));
                return None;
            }

            let hot_id = EventHotKeyId {
                signature: u32::from_be_bytes(*b"rmag"),
                id: 1,
            };
            let mut hotkey_ref: EventHotKeyRef = std::ptr::null_mut();
            let status = RegisterEventHotKey(KEY_V, modifiers, hot_id, target, 0, &mut hotkey_ref);
            if status != 0 {
                warn!(status, combo, "register clipboard hotkey failed");
                // 注册失败后须先移除 handler；若移除失败，保留 Sender 避免回调悬空。
                let remove_status = RemoveEventHandler(handler_ref);
                if remove_status == 0 {
                    drop(Box::from_raw(tx_ptr as *mut SyncSender<()>));
                } else {
                    warn!(
                        status = remove_status,
                        "RemoveEventHandler failed after hotkey registration failure; leaking callback sender for safety"
                    );
                }
                return None;
            }
            info!(combo, "global clipboard hotkey registered");
            Some(Self {
                rx,
                handler_ref: handler_ref as usize,
                hotkey_ref: hotkey_ref as usize,
                tx_ptr: tx_ptr as usize,
                combo,
            })
        }
    }

    /// 非阻塞取一次热键事件（多次触发只需知道是否发生过，故 drain 后返回是否有）
    pub fn poll(&self) -> bool {
        let mut fired = false;
        while self.rx.try_recv().is_ok() {
            fired = true;
        }
        fired
    }
}

impl Drop for HotkeyListener {
    /// 注销热键 → 移除 handler → 回收 Sender。须与注册同在主线程，避免与事件分发竞争。
    /// 先移除 handler 阻断后续回调，再释放其借用的 Sender
    fn drop(&mut self) {
        let cleaned = unsafe {
            let unregister_status = UnregisterEventHotKey(self.hotkey_ref as EventHotKeyRef);
            if unregister_status != 0 {
                warn!(
                    status = unregister_status,
                    "unregister clipboard hotkey failed"
                );
            }
            let remove_status = RemoveEventHandler(self.handler_ref as EventHandlerRef);
            if remove_status == 0 {
                drop(Box::from_raw(self.tx_ptr as *mut SyncSender<()>));
            } else {
                // handler 仍可能被 Carbon 调用，不能释放其借用的 Sender。
                warn!(
                    status = remove_status,
                    "RemoveEventHandler failed; leaking callback sender for safety"
                );
            }
            unregister_status == 0 && remove_status == 0
        };
        if cleaned {
            info!(combo = self.combo, "global clipboard hotkey unregistered");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_hotkey_events_are_coalesced() {
        let (tx, rx) = sync_channel(1);
        let user_data = (&tx as *const SyncSender<()>).cast_mut().cast::<c_void>();

        hotkey_handler(std::ptr::null_mut(), std::ptr::null_mut(), user_data);
        hotkey_handler(std::ptr::null_mut(), std::ptr::null_mut(), user_data);

        assert_eq!(rx.try_recv(), Ok(()));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn disconnected_hotkey_receiver_is_safe() {
        let (tx, rx) = sync_channel(1);
        drop(rx);
        let user_data = (&tx as *const SyncSender<()>).cast_mut().cast::<c_void>();

        assert_eq!(
            hotkey_handler(std::ptr::null_mut(), std::ptr::null_mut(), user_data),
            0
        );
    }
}
