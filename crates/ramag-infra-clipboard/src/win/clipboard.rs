//! Win32 剪贴板读写与变更序列号。

use std::ffi::c_void;

use windows::Win32::Foundation::{BOOL, GlobalFree, HANDLE, HGLOBAL, HWND, POINT, TRUE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows::core::{PCWSTR, w};

use ramag_domain::entities::CapturedClip;
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

use crate::dib::{dib_to_png, image_dimensions_allowed, png_dims, png_to_dib};
use crate::win::clipboard_owner::ClipboardOwner;

const OPEN_ATTEMPTS: usize = 8;
const OPEN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(5);
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLIPBOARD_FILES: u32 = 4_096;
const MAX_PATH_UNITS: usize = 32_767;

pub struct ClipboardRead {
    pub clip: CapturedClip,
    pub owner_hwnd: isize,
    pub owner_pid: u32,
}

/// 当前剪贴板变更序列号。
pub fn sequence_number() -> i64 {
    unsafe { GetClipboardSequenceNumber() as i64 }
}

/// OpenClipboard/CloseClipboard 的 RAII 守卫。
struct Clipboard;

impl Clipboard {
    fn open(owner: Option<HWND>) -> Result<Self> {
        let owner = owner.unwrap_or_default();
        for attempt in 0..OPEN_ATTEMPTS {
            match unsafe { OpenClipboard(owner) } {
                Ok(()) => return Ok(Self),
                Err(_) if attempt + 1 < OPEN_ATTEMPTS => {
                    std::thread::sleep(OPEN_RETRY_DELAY);
                    if attempt == 0 {
                        std::thread::yield_now();
                    }
                }
                Err(error) => {
                    return Err(DomainError::Other(format!(
                        "打开剪贴板失败（已重试 {OPEN_ATTEMPTS} 次）：{error}"
                    )));
                }
            }
        }
        Err(DomainError::Other("打开剪贴板失败".into()))
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseClipboard() } {
            warn!(operation = "clipboard_close", error = %error, "close clipboard failed");
        }
    }
}

/// 注册或获取 PNG 剪贴板格式 ID。
fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

/// Windows 约定的 RTF 剪贴板格式。
fn rtf_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("Rich Text Format")) }
}

fn format_available(format: u32) -> bool {
    format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

/// 从 HGLOBAL 句柄拷出字节，并限制单次分配。
unsafe fn global_bytes_limited(handle: HANDLE, max_bytes: usize) -> Option<Vec<u8>> {
    let hglobal = HGLOBAL(handle.0 as *mut c_void);
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return None;
    }
    let len = unsafe { GlobalSize(hglobal) };
    if len == 0 || len > max_bytes {
        unsafe {
            let _ = GlobalUnlock(hglobal);
        }
        return None;
    }
    let out = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec() };
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Some(out)
}

unsafe fn global_bytes(handle: HANDLE) -> Option<Vec<u8>> {
    unsafe { global_bytes_limited(handle, MAX_CLIPBOARD_BYTES) }
}

/// 剪贴板已打开时复制一种格式；多格式共享预算，防止恶意内容叠加分配。
fn copy_format(format: u32, remaining: &mut usize) -> Option<Vec<u8>> {
    if *remaining == 0 || !format_available(format) {
        return None;
    }
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    let bytes = unsafe { global_bytes_limited(handle, *remaining) }?;
    *remaining -= bytes.len();
    Some(bytes)
}

/// 把字节写入新分配的可移动 HGLOBAL，SetClipboardData 成功后所有权移交系统
unsafe fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    if format == 0 || bytes.is_empty() {
        return Err(DomainError::InvalidConfig("剪贴板格式或内容为空".into()));
    }
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "剪贴板内容超过 {} MiB 上限",
            MAX_CLIPBOARD_BYTES / 1024 / 1024
        )));
    }
    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
        .map_err(|e| DomainError::Other(format!("分配剪贴板内存失败：{e}")))?;
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        unsafe {
            let _ = GlobalFree(hglobal);
        }
        return Err(DomainError::Other("锁定剪贴板内存失败".into()));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(hglobal);
    }
    let handle = HANDLE(hglobal.0 as isize);
    if let Err(error) = unsafe { SetClipboardData(format, handle) } {
        unsafe {
            let _ = GlobalFree(hglobal);
        }
        return Err(DomainError::Other(format!("写剪贴板失败：{error}")));
    }
    Ok(())
}

/// 读取注册格式的 DWORD 值。
fn format_u32(format: u32) -> Option<u32> {
    if !format_available(format) {
        return None;
    }
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    let hglobal = HGLOBAL(handle.0 as *mut c_void);
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() || unsafe { GlobalSize(hglobal) } < std::mem::size_of::<u32>() {
        if !ptr.is_null() {
            unsafe {
                let _ = GlobalUnlock(hglobal);
            }
        }
        return None;
    }
    let mut value = [0u8; 4];
    unsafe {
        std::ptr::copy_nonoverlapping(ptr.cast::<u8>(), value.as_mut_ptr(), value.len());
        let _ = GlobalUnlock(hglobal);
    }
    Some(u32::from_le_bytes(value))
}

/// 识别系统和密码管理器的隐私标记。
fn is_concealed() -> bool {
    let exclude_monitor =
        unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
    let viewer_ignore = unsafe { RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")) };
    let apple_concealed = unsafe { RegisterClipboardFormatW(w!("org.nspasteboard.ConcealedType")) };
    let apple_transient = unsafe { RegisterClipboardFormatW(w!("org.nspasteboard.TransientType")) };
    if [
        exclude_monitor,
        viewer_ignore,
        apple_concealed,
        apple_transient,
    ]
    .into_iter()
    .any(format_available)
    {
        return true;
    }

    let include_history = unsafe { RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")) };
    matches!(format_u32(include_history), Some(0))
}

/// 累加 UTF-16 路径长度并预留 NUL。
fn checked_file_units(total: usize, path_units: usize) -> Option<usize> {
    if path_units == 0 || path_units > MAX_PATH_UNITS {
        return None;
    }
    let next = total.checked_add(path_units)?.checked_add(1)?;
    let wide_bytes = next.checked_add(1)?.checked_mul(2)?;
    let blob_bytes = std::mem::size_of::<DROPFILES>().checked_add(wide_bytes)?;
    (blob_bytes <= MAX_CLIPBOARD_BYTES).then_some(next)
}

fn read_file_list(handle: HANDLE) -> Option<Vec<String>> {
    let hdrop = HDROP(handle.0);
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    if count > MAX_CLIPBOARD_FILES {
        warn!(
            operation = "clipboard_file_list_parse",
            count, "ignore clipboard file list with too many entries"
        );
        return None;
    }

    let mut files = Vec::with_capacity(count as usize);
    let mut total_units = 0usize;
    for index in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, index, None) } as usize;
        if len == 0 {
            continue;
        }
        let Some(next_total) = checked_file_units(total_units, len) else {
            warn!(
                operation = "clipboard_file_list_parse",
                index, len, "ignore oversized clipboard file list"
            );
            return None;
        };
        let mut buffer = vec![0u16; len + 1];
        let copied = unsafe { DragQueryFileW(hdrop, index, Some(&mut buffer)) } as usize;
        if copied > len {
            warn!(
                operation = "clipboard_file_list_parse",
                index, copied, len, "ignore malformed clipboard file path"
            );
            return None;
        }
        if copied > 0 {
            files.push(String::from_utf16_lossy(&buffer[..copied]));
            total_units = next_total;
        }
    }
    Some(files)
}

/// 按文件、图片、文本优先级读取剪贴板。
pub fn read() -> Result<Option<ClipboardRead>> {
    let guard = Clipboard::open(None)?;
    let owner = unsafe { GetClipboardOwner() };
    let mut owner_pid = 0;
    unsafe { GetWindowThreadProcessId(owner, Some(&mut owner_pid)) };
    let finish = |clip| ClipboardRead {
        clip,
        owner_hwnd: owner.0,
        owner_pid,
    };
    if is_concealed() {
        return Ok(Some(finish(CapturedClip {
            concealed: true,
            ..Default::default()
        })));
    }
    let mut cap = CapturedClip::default();

    if unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_ok()
        && let Ok(handle) = unsafe { GetClipboardData(CF_HDROP.0 as u32) }
        && let Some(files) = read_file_list(handle)
    {
        cap.files = files;
        if !cap.files.is_empty() {
            return Ok(Some(finish(cap)));
        }
    }

    let png_fmt = png_format();
    if unsafe { IsClipboardFormatAvailable(png_fmt) }.is_ok()
        && let Ok(handle) = unsafe { GetClipboardData(png_fmt) }
        && let Some(bytes) = unsafe { global_bytes(handle) }
        && !bytes.is_empty()
    {
        if let Some(dims) = png_dims(&bytes).filter(|dims| image_dimensions_allowed(*dims)) {
            cap.image_dims = Some(dims);
            cap.image_png = Some(bytes);
            return Ok(Some(finish(cap)));
        }
        warn!(
            operation = "clipboard_image_parse",
            "ignore malformed or oversized PNG clipboard format"
        );
    }

    // 先复制格式，再关闭剪贴板后解码图片。
    let mut remaining = MAX_CLIPBOARD_BYTES;
    let mut dib_candidates = Vec::with_capacity(2);
    for format in [CF_DIBV5.0 as u32, CF_DIB.0 as u32] {
        if let Some(dib) = copy_format(format, &mut remaining) {
            dib_candidates.push(dib);
        }
    }
    let text_bytes = copy_format(CF_UNICODETEXT.0 as u32, &mut remaining);
    let rtf_bytes = text_bytes.as_ref().and_then(|_| {
        let format = rtf_format();
        copy_format(format, &mut remaining)
    });
    drop(guard);

    // 将 CF_DIBV5/CF_DIB 转为 PNG。
    for dib in dib_candidates {
        if let Some(png) = dib_to_png(&dib)
            && let Some(dims) = png_dims(&png).filter(|dims| image_dimensions_allowed(*dims))
        {
            cap.image_dims = Some(dims);
            cap.image_png = Some(png);
            return Ok(Some(finish(cap)));
        }
    }

    if let Some(bytes) = text_bytes {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        if !units.is_empty() {
            cap.text = Some(String::from_utf16_lossy(&units));
            if let Some(mut bytes) = rtf_bytes {
                while bytes.last() == Some(&0) {
                    bytes.pop();
                }
                if !bytes.is_empty() {
                    cap.rtf = Some(bytes);
                }
            }
            return Ok(Some(finish(cap)));
        }
    }

    Ok(None)
}

/// 写入 UTF-16 文本，可附带 RTF。
pub fn write_text(text: &str, rtf: Option<&[u8]>) -> Result<i64> {
    if text.contains('\0') {
        return Err(DomainError::InvalidConfig(
            "文本包含 NUL 字符，无法写入 Windows 剪贴板".into(),
        ));
    }
    let encoded_bytes = text
        .encode_utf16()
        .count()
        .checked_add(1)
        .and_then(|units| units.checked_mul(2))
        .ok_or_else(|| DomainError::InvalidConfig("剪贴文本长度溢出".into()))?;
    if encoded_bytes > MAX_CLIPBOARD_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "剪贴文本超过 {} MiB 上限",
            MAX_CLIPBOARD_BYTES / 1024 / 1024
        )));
    }
    if rtf.is_some_and(|bytes| bytes.len().saturating_add(1) > MAX_CLIPBOARD_BYTES) {
        return Err(DomainError::InvalidConfig(format!(
            "RTF 内容超过 {} MiB 上限",
            MAX_CLIPBOARD_BYTES / 1024 / 1024
        )));
    }
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes = unsafe { std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), units.len() * 2) };
    // owner 需在剪贴板关闭后销毁。
    let owner = ClipboardOwner::create()?;
    let _guard = Clipboard::open(Some(owner.hwnd()))?;
    unsafe {
        EmptyClipboard().map_err(|e| DomainError::Other(format!("清空剪贴板失败：{e}")))?;
        // 按丰富度顺序注册，使应用优先选择 RTF。
        if let Some(rtf) = rtf.filter(|bytes| !bytes.is_empty()) {
            let mut terminated = Vec::with_capacity(rtf.len() + 1);
            terminated.extend_from_slice(rtf);
            if terminated.last() != Some(&0) {
                terminated.push(0);
            }
            let format = rtf_format();
            if let Err(error) = set_clipboard_bytes(format, &terminated) {
                warn!(operation = "clipboard_write_optional_format", error = %error, "write optional RTF clipboard format failed");
            }
        }
        set_clipboard_bytes(CF_UNICODETEXT.0 as u32, bytes)?;
        Ok(sequence_number())
    }
}

/// 写入 PNG，并尽量附带 CF_DIB 兼容外部应用。
pub fn write_image_png(png: &[u8]) -> Result<i64> {
    if png.len() > MAX_CLIPBOARD_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "PNG 图片超过 {} MiB 上限",
            MAX_CLIPBOARD_BYTES / 1024 / 1024
        )));
    }
    let _dims = png_dims(png)
        .filter(|dims| image_dimensions_allowed(*dims))
        .ok_or_else(|| DomainError::InvalidConfig("PNG 图片格式无效或尺寸过大".into()))?;
    // 转码在打开剪贴板前完成，避免长时间占锁。
    let dib = png_to_dib(png);
    let fmt = png_format();
    let owner = ClipboardOwner::create()?;
    let _guard = Clipboard::open(Some(owner.hwnd()))?;
    unsafe {
        EmptyClipboard().map_err(|e| DomainError::Other(format!("清空剪贴板失败：{e}")))?;
        set_clipboard_bytes(fmt, png)?;
        // 附加 CF_DIB 失败不影响已写入的 PNG。
        if let Some(dib) = dib
            && let Err(error) = set_clipboard_bytes(CF_DIB.0 as u32, &dib)
        {
            warn!(operation = "clipboard_write_optional_format", error = %error, "write optional CF_DIB clipboard format failed");
        }
        Ok(sequence_number())
    }
}

/// 写入 CF_HDROP 文件列表。
pub fn write_files(paths: &[String]) -> Result<i64> {
    if paths.is_empty() || paths.len() > MAX_CLIPBOARD_FILES as usize {
        return Err(DomainError::InvalidConfig(
            "文件列表为空或文件数量过多".into(),
        ));
    }
    let mut wide: Vec<u16> = Vec::new();
    let mut total_units = 0usize;
    for p in paths {
        if p.is_empty() || p.contains('\0') {
            return Err(DomainError::InvalidConfig(
                "文件路径为空或包含 NUL 字符".into(),
            ));
        }
        let path_units = p.encode_utf16().count();
        total_units = checked_file_units(total_units, path_units)
            .ok_or_else(|| DomainError::InvalidConfig("文件路径过长或文件列表内容过大".into()))?;
        wide.extend(p.encode_utf16());
        wide.push(0);
    }
    wide.push(0);

    let header = DROPFILES {
        pFiles: std::mem::size_of::<DROPFILES>() as u32,
        pt: POINT { x: 0, y: 0 },
        fNC: BOOL(0),
        fWide: TRUE,
    };
    let mut blob: Vec<u8> = Vec::with_capacity(std::mem::size_of::<DROPFILES>() + wide.len() * 2);
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&header as *const DROPFILES).cast::<u8>(),
            std::mem::size_of::<DROPFILES>(),
        )
    };
    blob.extend_from_slice(header_bytes);
    let wide_bytes =
        unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    blob.extend_from_slice(wide_bytes);

    let owner = ClipboardOwner::create()?;
    let _guard = Clipboard::open(Some(owner.hwnd()))?;
    unsafe {
        EmptyClipboard().map_err(|e| DomainError::Other(format!("清空剪贴板失败：{e}")))?;
        set_clipboard_bytes(CF_HDROP.0 as u32, &blob)?;
        Ok(sequence_number())
    }
}

/// 构造 NUL 结尾的宽字符串。
pub fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 将宽字符串首指针包装为 PCWSTR。
pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR(buf.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATH_UNITS, checked_file_units};

    #[test]
    fn clipboard_resource_limits_are_checked() {
        assert!(checked_file_units(0, 10).is_some());
        assert!(checked_file_units(0, 0).is_none());
        assert!(checked_file_units(0, MAX_PATH_UNITS + 1).is_none());
    }
}
