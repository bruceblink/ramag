//! 前台应用标注（GetForegroundWindow → 进程 exe）/ 模拟粘贴（SendInput Ctrl-V）/
//! 打开链接（ShellExecuteW）/ 资源管理器选中（explorer /select）。

use std::mem::size_of;

use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_V,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{PWSTR, w};

use ramag_domain::entities::ClipSource;
use ramag_domain::error::{DomainError, Result};

use crate::win::clipboard::{pcwstr, wide_nul};

/// 前台窗口所属进程的可执行文件路径与名字（作为来源标注）
pub fn frontmost_app() -> Option<ClipSource> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        ok.ok()?;
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        let name = full
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&full)
            .trim_end_matches(".exe")
            .to_string();
        Some(ClipSource {
            bundle_id: full,
            name,
        })
    }
}

/// 一个键的按下 / 抬起 INPUT
fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// 模拟按下 Ctrl-V（粘贴）。延迟由调用方在后台线程控制
pub fn send_ctrl_v() {
    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_CONTROL, true),
    ];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// 后台线程延迟发 Ctrl-V：等前台切换到位，且不阻塞主线程
pub fn post_ctrl_v_delayed(delay_ms: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        send_ctrl_v();
    });
}

/// 默认浏览器打开链接
pub fn open_url(url: &str) -> Result<()> {
    let wide = wide_nul(url);
    let hinst = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            pcwstr(&wide),
            None,
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW 成功返回值 > 32
    if hinst.0 as isize > 32 {
        Ok(())
    } else {
        Err(DomainError::Other(format!("打开链接失败：{url}")))
    }
}

/// 在资源管理器中选中文件（每个路径开一个窗口选中）
pub fn reveal_in_explorer(paths: &[String]) -> Result<()> {
    for p in paths {
        std::process::Command::new("explorer")
            .arg(format!("/select,{p}"))
            .spawn()
            .map_err(|e| DomainError::Other(format!("打开资源管理器失败：{e}")))?;
    }
    Ok(())
}
