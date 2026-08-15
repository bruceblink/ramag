//! 全局热键：Carbon 注册剪贴板抽屉与 Ramag 主窗口唤醒组合键。
//! 事件回调在主线程触发，经有界异步 channel 转出；接收端可直接 await，避免后台轮询
//! 在应用长期不活跃后被系统降频。

use std::ffi::c_void;

use async_channel::{Receiver, Sender, TrySendError, bounded};

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
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: u32,
        actual_size: *mut u32,
        data: *mut c_void,
    ) -> OsStatus;
}

// Carbon 常量：kEventClassKeyboard = 'keyb'，kEventHotKeyPressed = 5。
const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_HOTKEY_PRESSED: u32 = 5;
const EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_EVENT_HOTKEY_ID: u32 = u32::from_be_bytes(*b"hkid");
// Carbon 修饰键掩码
const CMD_KEY: u32 = 0x0100;
const SHIFT_KEY: u32 = 0x0200;
const OPTION_KEY: u32 = 0x0800;
// V 键码为 9。
const KEY_V: u32 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    ClipboardDrawer,
    WakeMainWindow,
}

fn send_event(tx: &Sender<HotkeyEvent>, event: HotkeyEvent) {
    match tx.try_send(event) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Closed(_)) => {}
    }
}

/// 热键事件回调：经 user_data 还原 Sender 并发信号。回调全程不 panic（跨 FFI 边界）
extern "C" fn hotkey_handler(
    _next: EventHandlerCallRef,
    event: EventRef,
    user_data: *mut c_void,
) -> OsStatus {
    if user_data.is_null() || event.is_null() {
        return 0;
    }
    let mut hotkey_id = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    let status = unsafe {
        GetEventParameter(
            event,
            EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOTKEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>() as u32,
            std::ptr::null_mut(),
            (&mut hotkey_id as *mut EventHotKeyId).cast(),
        )
    };
    if status == 0 {
        let event = match hotkey_id.id {
            1 => Some(HotkeyEvent::ClipboardDrawer),
            2 => Some(HotkeyEvent::WakeMainWindow),
            _ => None,
        };
        if let Some(event) = event {
            let tx = unsafe { &*(user_data as *const Sender<HotkeyEvent>) };
            send_event(tx, event);
        }
    }
    0
}

/// 热键句柄：持有 Receiver 与 Carbon ref；Drop 时注销热键、移除 handler、回收 Sender。
/// ref 以 usize 存（裸指针非 Send，须能随句柄移入异步轮询任务）
pub struct HotkeyListener {
    rx: Receiver<HotkeyEvent>,
    handler_ref: usize,
    clipboard_ref: Option<usize>,
    wake_ref: Option<usize>,
    tx_ptr: usize,
}

impl HotkeyListener {
    /// 固定注册主窗口唤醒键；剪贴板启用时再注册抽屉键。
    /// 须在主线程、NSApplication 事件循环就绪后调用
    pub fn register_clipboard_hotkey(alternate: bool, clipboard_enabled: bool) -> Option<Self> {
        let (modifiers, combo) = if alternate {
            (CMD_KEY | OPTION_KEY, "cmd-alt-v")
        } else {
            (CMD_KEY | SHIFT_KEY, "cmd-shift-v")
        };
        let (tx, rx) = bounded::<HotkeyEvent>(8);
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
                warn!(
                    operation = "clipboard_hotkey_install",
                    status, "install clipboard hotkey event handler failed"
                );
                drop(Box::from_raw(tx_ptr as *mut Sender<HotkeyEvent>));
                return None;
            }

            let mut clipboard_ref: EventHotKeyRef = std::ptr::null_mut();
            let clipboard_status = if clipboard_enabled {
                RegisterEventHotKey(
                    KEY_V,
                    modifiers,
                    EventHotKeyId {
                        signature: u32::from_be_bytes(*b"rmag"),
                        id: 1,
                    },
                    target,
                    0,
                    &mut clipboard_ref,
                )
            } else {
                -1
            };
            if clipboard_enabled && clipboard_status != 0 {
                warn!(
                    operation = "clipboard_hotkey_register",
                    status = clipboard_status,
                    combo,
                    "register clipboard hotkey failed"
                );
            }

            let mut wake_ref: EventHotKeyRef = std::ptr::null_mut();
            let wake_status = RegisterEventHotKey(
                KEY_V,
                CMD_KEY | OPTION_KEY | SHIFT_KEY,
                EventHotKeyId {
                    signature: u32::from_be_bytes(*b"rmag"),
                    id: 2,
                },
                target,
                0,
                &mut wake_ref,
            );
            if wake_status != 0 {
                warn!(
                    operation = "main_window_hotkey_register",
                    status = wake_status,
                    "register main window hotkey failed"
                );
            }

            let clipboard_ref = (clipboard_status == 0).then_some(clipboard_ref as usize);
            let wake_ref = (wake_status == 0).then_some(wake_ref as usize);
            if clipboard_ref.is_none() && wake_ref.is_none() {
                let remove_status = RemoveEventHandler(handler_ref);
                if remove_status == 0 {
                    drop(Box::from_raw(tx_ptr as *mut Sender<HotkeyEvent>));
                }
                return None;
            }
            info!(
                operation = "global_hotkey_register",
                clipboard_enabled, combo, "global hotkeys registered"
            );
            Some(Self {
                rx,
                handler_ref: handler_ref as usize,
                clipboard_ref,
                wake_ref,
                tx_ptr: tx_ptr as usize,
            })
        }
    }

    pub fn clipboard_registered(&self) -> bool {
        self.clipboard_ref.is_some()
    }

    pub fn poll(&self) -> Option<HotkeyEvent> {
        self.rx.try_recv().ok()
    }

    /// 等待下一个热键事件；事件到达会直接唤醒接收任务。
    pub async fn recv(&self) -> Option<HotkeyEvent> {
        self.rx.recv().await.ok()
    }
}

impl Drop for HotkeyListener {
    /// 注销热键 → 移除 handler → 回收 Sender。须与注册同在主线程，避免与事件分发竞争。
    /// 先移除 handler 阻断后续回调，再释放其借用的 Sender
    fn drop(&mut self) {
        let cleaned = unsafe {
            let mut unregister_ok = true;
            for hotkey_ref in [self.clipboard_ref, self.wake_ref].into_iter().flatten() {
                if UnregisterEventHotKey(hotkey_ref as EventHotKeyRef) != 0 {
                    unregister_ok = false;
                }
            }
            let remove_status = RemoveEventHandler(self.handler_ref as EventHandlerRef);
            if remove_status == 0 {
                drop(Box::from_raw(self.tx_ptr as *mut Sender<HotkeyEvent>));
            } else {
                // handler 仍可能被 Carbon 调用，不能释放其借用的 Sender。
                warn!(
                    operation = "clipboard_hotkey_cleanup",
                    status = remove_status,
                    "RemoveEventHandler failed; leaking callback sender for safety"
                );
            }
            unregister_ok && remove_status == 0
        };
        if cleaned {
            info!(
                operation = "global_hotkey_unregister",
                "global hotkeys unregistered"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_hotkey_events_are_coalesced() {
        let (tx, rx) = bounded(1);
        send_event(&tx, HotkeyEvent::WakeMainWindow);
        send_event(&tx, HotkeyEvent::WakeMainWindow);
        assert_eq!(rx.try_recv(), Ok(HotkeyEvent::WakeMainWindow));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn disconnected_hotkey_receiver_is_safe() {
        let (tx, rx) = bounded::<HotkeyEvent>(1);
        drop(rx);
        send_event(&tx, HotkeyEvent::ClipboardDrawer);
    }
}
