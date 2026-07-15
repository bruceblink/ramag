//! 粘贴模拟：辅助功能权限检测 + 激活目标应用 + CGEvent 发 cmd-V

use cocoa::base::{id, nil};
use cocoa::foundation::NSArray;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc::{class, msg_send, sel, sel_impl};
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

use crate::pasteboard::ns_string;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

/// prompt=true 时系统会弹「辅助功能」授权引导窗
pub(crate) fn accessibility_trusted(prompt: bool) -> bool {
    unsafe {
        if !prompt {
            return AXIsProcessTrusted();
        }
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let dict = CFDictionary::from_CFType_pairs(&[(
            key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);
        AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef())
    }
}

/// 激活 bundle_id 对应的运行中应用；返回目标 pid，未运行或激活失败返回 None。
pub(crate) fn activate_app(bundle_id: &str) -> Option<i32> {
    unsafe {
        let arr: id = msg_send![class!(NSRunningApplication),
            runningApplicationsWithBundleIdentifier: ns_string(bundle_id)];
        if arr == nil || NSArray::count(arr) == 0 {
            return None;
        }
        let app: id = NSArray::objectAtIndex(arr, 0);
        let pid: i32 = msg_send![app, processIdentifier];
        // NSApplicationActivateIgnoringOtherApps（已软废弃但行为可靠）
        let activated: bool = msg_send![app, activateWithOptions: 1u64 << 1];
        (activated && pid > 0).then_some(pid)
    }
}

/// 后台线程延迟发 cmd-V：等待激活切换到位；发送前复核目标 pid，
/// 防止用户切窗后把敏感内容粘到其它应用。
pub(crate) fn post_cmd_v_delayed(delay_ms: u64, expected_pid: i32) -> Result<()> {
    std::thread::Builder::new()
        .name("ramag-clipboard-paste".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            if frontmost_pid() != Some(expected_pid) {
                warn!(
                    expected_pid,
                    "skip cmd-v because target app is no longer foreground"
                );
                return;
            }
            post_cmd_v();
        })
        .map(|_| ())
        .map_err(|error| DomainError::Other(format!("启动自动粘贴线程失败：{error}")))
}

fn frontmost_pid() -> Option<i32> {
    unsafe {
        // 后台线程上的 Cocoa 调用需要自己的 autorelease pool。
        let pool: id = msg_send![class!(NSAutoreleasePool), new];
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: id = msg_send![workspace, frontmostApplication];
        let pid = if app == nil {
            None
        } else {
            let value: i32 = msg_send![app, processIdentifier];
            (value > 0).then_some(value)
        };
        let _: () = msg_send![pool, drain];
        pid
    }
}

fn post_cmd_v() {
    // kVK_ANSI_V = 9
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) else {
        warn!("create CGEventSource failed");
        return;
    };
    for down in [true, false] {
        match CGEvent::new_keyboard_event(src.clone(), 9, down) {
            Ok(ev) => {
                ev.set_flags(CGEventFlags::CGEventFlagCommand);
                ev.post(CGEventTapLocation::HID);
            }
            Err(()) => warn!(down, "create cmd-v keyboard event failed"),
        }
    }
}
