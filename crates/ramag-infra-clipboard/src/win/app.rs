//! 前台应用标注与恢复 / 应用图标 / 模拟粘贴 / 打开链接 / 资源管理器选中。

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, DIB_RGB_COLORS, DeleteObject, EnumDisplayMonitors, GetDC,
    GetDIBits, GetObjectW, HBITMAP, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow,
    ReleaseDC,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetForegroundWindow, GetIconInfo, GetWindowThreadProcessId, HICON, ICONINFO,
    IsIconic, IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindowAsync,
};
use windows::core::PWSTR;

use ramag_domain::entities::ClipSource;
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

use crate::win::clipboard::{pcwstr, wide_nul};

const MAX_PROCESS_PATH_UNITS: usize = 32_768;
const MAX_ICON_DIMENSION: i32 = 1_024;
const MAX_ICON_PIXELS: usize = 1_024 * 1_024;
const WINDOW_TOKEN_PREFIX: &str = "win32-hwnd:";
const MODIFIER_WAIT_STEPS: usize = 25;
const MODIFIER_WAIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

struct MonitorLookup {
    target: HMONITOR,
    index: usize,
    found: Option<usize>,
}

unsafe extern "system" fn monitor_enum_proc(
    monitor: HMONITOR,
    _dc: HDC,
    _bounds: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let lookup = unsafe { &mut *(data.0 as *mut MonitorLookup) };
    if monitor.0 == lookup.target.0 {
        lookup.found = Some(lookup.index);
    }
    lookup.index += 1;
    BOOL(1)
}

/// 前台窗口所在显示器在 EnumDisplayMonitors 中的序号；GPUI Windows 使用相同枚举顺序。
pub fn foreground_display_index() -> Option<usize> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 == 0 {
        return None;
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return None;
    }
    let mut lookup = MonitorLookup {
        target: monitor,
        index: 0,
        found: None,
    };
    let enumerated = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_enum_proc),
            LPARAM((&mut lookup as *mut MonitorLookup) as isize),
        )
    };
    enumerated.as_bool().then_some(lookup.found).flatten()
}

fn icon_dimensions_allowed(width: i32, height: i32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_ICON_DIMENSION
        && height <= MAX_ICON_DIMENSION
        && usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .is_some_and(|pixels| pixels <= MAX_ICON_PIXELS)
}

/// 查询进程 exe 完整路径。使用 Win32 长路径上限，避免 MAX_PATH 截断。
fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; MAX_PROCESS_PATH_UNITS];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        result.ok()?;
        if size as usize > buf.len() {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

fn app_for_window(hwnd: HWND, expected_pid: Option<u32>) -> Option<ClipSource> {
    if hwnd.0 == 0 {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 || expected_pid.is_some_and(|expected| expected != pid) {
        return None;
    }
    let full = process_path(pid)?;
    let name = std::path::Path::new(&full)
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| full.clone());
    Some(ClipSource {
        bundle_id: full,
        name,
    })
}

pub fn app_for_window_handle(raw: isize, expected_pid: u32) -> Option<ClipSource> {
    app_for_window(HWND(raw), Some(expected_pid))
}

/// 前台窗口所属进程的可执行文件路径与名字（作为来源标注）
pub fn frontmost_app() -> Option<ClipSource> {
    app_for_window(unsafe { GetForegroundWindow() }, None)
}

/// 记录热键触发时的精确前台窗口；短期 token 仅用于抽屉关闭后恢复焦点。
pub fn foreground_window_token() -> Option<String> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 == 0 {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then(|| format!("{WINDOW_TOKEN_PREFIX}{:x}:{pid:x}", hwnd.0 as usize))
}

/// 恢复 token 对应的窗口。窗口已关闭或系统拒绝前台切换时显式报错。
fn parse_window_token(token: &str) -> Result<(usize, u32)> {
    let raw = token
        .strip_prefix(WINDOW_TOKEN_PREFIX)
        .ok_or_else(|| DomainError::InvalidConfig("无效的 Windows 窗口标识".into()))?;
    let (window_raw, pid_raw) = raw
        .split_once(':')
        .ok_or_else(|| DomainError::InvalidConfig("无效的 Windows 窗口标识".into()))?;
    let value = usize::from_str_radix(window_raw, 16)
        .map_err(|e| DomainError::InvalidConfig(format!("无效的 Windows 窗口标识：{e}")))?;
    let expected_pid = u32::from_str_radix(pid_raw, 16)
        .map_err(|e| DomainError::InvalidConfig(format!("无效的 Windows 进程标识：{e}")))?;
    if value == 0 || expected_pid == 0 {
        return Err(DomainError::InvalidConfig(
            "Windows 窗口或进程标识不能为零".into(),
        ));
    }
    Ok((value, expected_pid))
}

/// 延迟发送按键前再次核对前台窗口，避免用户切窗后把敏感内容粘到错误应用。
fn target_is_foreground(value: usize, expected_pid: u32) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0 as usize != value {
        return false;
    }
    let mut current_pid = 0u32;
    unsafe { GetWindowThreadProcessId(foreground, Some(&mut current_pid)) };
    current_pid == expected_pid
}

/// 恢复 token 对应的窗口。窗口已关闭或系统拒绝前台切换时显式报错。
pub fn activate_window_token(token: &str) -> Result<()> {
    let (value, expected_pid) = parse_window_token(token)?;
    let hwnd = HWND(value as isize);
    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return Err(DomainError::NotFound(
                "原窗口已关闭，内容已复制到剪贴板".into(),
            ));
        }
        let mut current_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut current_pid));
        if current_pid == 0 || current_pid != expected_pid {
            return Err(DomainError::NotFound(
                "原窗口已失效，内容已复制到剪贴板".into(),
            ));
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            return Err(DomainError::Other(
                "Windows 阻止切回原窗口，内容已复制到剪贴板".into(),
            ));
        }
    }
    Ok(())
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

/// 模拟按下 Ctrl-V；返回值不足说明系统拒绝了部分输入。
fn send_ctrl_v() -> Result<()> {
    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
    if sent == inputs.len() {
        Ok(())
    } else {
        // 极少数部分写入场景也必须补抬键，避免 Ctrl 或 V 停留在按下状态。
        if sent > 0 {
            let releases = [key_input(VK_V, true), key_input(VK_CONTROL, true)];
            let released = unsafe { SendInput(&releases, size_of::<INPUT>() as i32) } as usize;
            if released != releases.len() {
                warn!(
                    released,
                    expected = releases.len(),
                    "release partial paste keys failed"
                );
            }
        }
        Err(DomainError::Other(format!(
            "模拟 Ctrl+V 失败：仅发送 {sent}/{} 个输入事件",
            inputs.len()
        )))
    }
}

fn wait_for_modifier_release() -> bool {
    let modifiers = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN];
    for _ in 0..MODIFIER_WAIT_STEPS {
        let pressed = modifiers
            .iter()
            .any(|key| unsafe { GetAsyncKeyState(key.0 as i32) } < 0);
        if !pressed {
            return true;
        }
        std::thread::sleep(MODIFIER_WAIT_INTERVAL);
    }
    false
}

/// 后台线程延迟发 Ctrl-V：等前台切换到位，且不阻塞主线程
pub fn post_ctrl_v_delayed(delay_ms: u64, target: &str) -> Result<()> {
    let (window, pid) = parse_window_token(target)?;
    std::thread::Builder::new()
        .name("ramag-clipboard-paste".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            if !wait_for_modifier_release() {
                warn!("skip ctrl-v because a modifier key is still pressed");
                return;
            }
            if !target_is_foreground(window, pid) {
                warn!("skip ctrl-v because original window is no longer foreground");
                return;
            }
            if let Err(e) = send_ctrl_v() {
                warn!(error = %e, "send ctrl-v failed");
            }
        })
        .map(|_| ())
        .map_err(|e| DomainError::Other(format!("启动自动粘贴线程失败：{e}")))
}

struct IconGuard(HICON);

impl Drop for IconGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DestroyIcon(self.0);
            }
        }
    }
}

struct IconBitmaps {
    color: HBITMAP,
    mask: HBITMAP,
}

impl Drop for IconBitmaps {
    fn drop(&mut self) {
        unsafe {
            if !self.color.is_invalid() {
                let _ = DeleteObject(self.color);
            }
            if !self.mask.is_invalid() {
                let _ = DeleteObject(self.mask);
            }
        }
    }
}

/// 以 32-bit BGRA 读取 HBITMAP，并要求输出为自上而下的像素顺序。
fn bitmap_bgra(bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
    if !icon_dimensions_allowed(width, height) {
        return None;
    }
    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB.0;

    let len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    let mut pixels = vec![0u8; len];
    let hdc = unsafe { GetDC(None) };
    if hdc.is_invalid() {
        return None;
    }
    let lines = unsafe {
        GetDIBits(
            hdc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast::<c_void>()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    unsafe {
        let _ = ReleaseDC(None, hdc);
    }
    (lines == height).then_some(pixels)
}

/// 从 exe 资源提取首个大图标并转换为 PNG。
pub fn app_icon_png(exe_path: &str) -> Option<Vec<u8>> {
    let path = wide_nul(exe_path);
    let mut icon = HICON::default();
    if unsafe { ExtractIconExW(pcwstr(&path), 0, Some(&mut icon), None, 1) } == 0
        || icon.is_invalid()
    {
        return None;
    }
    let _icon = IconGuard(icon);

    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;
    let bitmaps = IconBitmaps {
        color: info.hbmColor,
        mask: info.hbmMask,
    };
    if bitmaps.color.is_invalid() {
        return None;
    }

    let mut meta = BITMAP::default();
    let read = unsafe {
        GetObjectW(
            bitmaps.color,
            size_of::<BITMAP>() as i32,
            Some((&mut meta as *mut BITMAP).cast::<c_void>()),
        )
    };
    if read == 0 || meta.bmWidth <= 0 || meta.bmHeight == 0 {
        return None;
    }
    let width = meta.bmWidth;
    let height = meta.bmHeight.checked_abs()?;
    let color = bitmap_bgra(bitmaps.color, width, height)?;
    let mask = (!bitmaps.mask.is_invalid())
        .then(|| bitmap_bgra(bitmaps.mask, width, height))
        .flatten();
    let has_alpha = color.chunks_exact(4).any(|pixel| pixel[3] != 0);

    let mut rgba = Vec::with_capacity(color.len());
    for (index, pixel) in color.chunks_exact(4).enumerate() {
        let alpha = if has_alpha {
            pixel[3]
        } else {
            mask.as_ref()
                .and_then(|bytes| bytes.get(index * 4..index * 4 + 3))
                .map_or(255, |mask_pixel| {
                    if mask_pixel.iter().any(|channel| *channel != 0) {
                        0
                    } else {
                        255
                    }
                })
        };
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
    }

    let image = image::RgbaImage::from_raw(width as u32, height as u32, rgba)?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::{icon_dimensions_allowed, parse_window_token};

    #[test]
    fn window_token_requires_handle_and_process() {
        assert_eq!(
            parse_window_token("win32-hwnd:ff:10").expect("valid token"),
            (0xff, 0x10)
        );
        assert!(parse_window_token("win32-hwnd:ff").is_err());
        assert!(parse_window_token("win32-hwnd:0:10").is_err());
        assert!(parse_window_token("other:ff:10").is_err());
    }

    #[test]
    fn icon_dimensions_are_bounded() {
        assert!(icon_dimensions_allowed(256, 256));
        assert!(icon_dimensions_allowed(1_024, 1_024));
        assert!(!icon_dimensions_allowed(0, 256));
        assert!(!icon_dimensions_allowed(1_025, 1));
    }
}
