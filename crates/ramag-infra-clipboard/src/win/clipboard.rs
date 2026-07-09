//! Win32 剪贴板读写：文本（CF_UNICODETEXT）/ 文件（CF_HDROP）/ 图片（注册 "PNG" 格式）。
//! changeCount 用 GetClipboardSequenceNumber（与 macOS changeCount 同义，轮询比对）。

use std::ffi::c_void;

use windows::Win32::Foundation::{BOOL, HANDLE, HGLOBAL, POINT, TRUE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::core::{PCWSTR, w};

use ramag_domain::entities::CapturedClip;
use ramag_domain::error::{DomainError, Result};

/// 当前剪贴板序列号（系统级单调递增，任何内容变更都会自增）
pub fn sequence_number() -> i64 {
    unsafe { GetClipboardSequenceNumber() as i64 }
}

/// OpenClipboard/CloseClipboard 的 RAII 守卫，确保任何路径退出都关闭
struct Clipboard;

impl Clipboard {
    fn open() -> Result<Self> {
        unsafe { OpenClipboard(None) }
            .map_err(|e| DomainError::Other(format!("打开剪贴板失败：{e}")))?;
        Ok(Self)
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// 注册（或取回已注册的）"PNG" 剪贴板格式 id
fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

/// 从 HGLOBAL 句柄拷出全部字节
unsafe fn global_bytes(handle: HANDLE) -> Option<Vec<u8>> {
    let hglobal = HGLOBAL(handle.0 as *mut c_void);
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return None;
    }
    let len = unsafe { GlobalSize(hglobal) };
    let out = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec() };
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Some(out)
}

/// 把字节写入新分配的可移动 HGLOBAL，SetClipboardData 成功后所有权移交系统
unsafe fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
        .map_err(|e| DomainError::Other(format!("分配剪贴板内存失败：{e}")))?;
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return Err(DomainError::Other("锁定剪贴板内存失败".into()));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(hglobal);
    }
    let handle = HANDLE(hglobal.0 as isize);
    unsafe { SetClipboardData(format, handle) }
        .map_err(|e| DomainError::Other(format!("写剪贴板失败：{e}")))?;
    Ok(())
}

/// PNG IHDR 固定偏移取宽高（与 macOS 侧同款）
fn png_dims(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || png.get(1..4)? != b"PNG" {
        return None;
    }
    let w = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

/// CF_DIB（无文件头的 BMP，截图常见）→ PNG：补 14 字节 BITMAPFILEHEADER 后交 image 解码再编 PNG。
/// 像素数据偏移 = 14 + 信息头大小 + 调色板/位域掩码大小
fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let bi_size = u32::from_le_bytes(dib.get(0..4)?.try_into().ok()?);
    let bit_count = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32::from_le_bytes(dib.get(16..20)?.try_into().ok()?);
    let clr_used = u32::from_le_bytes(dib.get(32..36)?.try_into().ok()?);
    // 调色板（<=8bpp）或 BI_BITFIELDS（compression=3，仅 40 字节头需额外 3×4 掩码）占用
    let table = if bit_count <= 8 {
        let n = if clr_used != 0 {
            clr_used
        } else {
            1u32 << bit_count
        };
        n * 4
    } else if compression == 3 && bi_size == 40 {
        12
    } else {
        0
    };
    let pixel_offset = 14 + bi_size + table;
    let file_size = 14 + dib.len() as u32;
    let mut bmp = Vec::with_capacity(dib.len() + 14);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    let img = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp).ok()?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// PNG → CF_DIB：编码成 BMP 后去掉 14 字节文件头得到裸 DIB（让图能粘进 Paint / Word）
fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let mut bmp = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bmp), image::ImageFormat::Bmp)
        .ok()?;
    if bmp.len() > 14 {
        Some(bmp[14..].to_vec())
    } else {
        None
    }
}

/// 读当前剪贴板。优先级：文件 > 图片（PNG）> 文本
pub fn read() -> Result<Option<CapturedClip>> {
    let _guard = Clipboard::open()?;
    let mut cap = CapturedClip::default();

    // 文件：CF_HDROP
    if unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_ok()
        && let Ok(handle) = unsafe { GetClipboardData(CF_HDROP.0 as u32) }
    {
        let hdrop = HDROP(handle.0);
        let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
        for i in 0..count {
            let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            let got = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) } as usize;
            if got > 0 {
                cap.files.push(String::from_utf16_lossy(&buf[..got]));
            }
        }
        if !cap.files.is_empty() {
            return Ok(Some(cap));
        }
    }

    // 图片：注册的 "PNG" 格式（Ramag 自身与多数现代应用走此格式）
    let png_fmt = png_format();
    if unsafe { IsClipboardFormatAvailable(png_fmt) }.is_ok()
        && let Ok(handle) = unsafe { GetClipboardData(png_fmt) }
        && let Some(bytes) = unsafe { global_bytes(handle) }
        && !bytes.is_empty()
    {
        cap.image_dims = png_dims(&bytes);
        cap.image_png = Some(bytes);
        return Ok(Some(cap));
    }

    // 图片：CF_DIB（截图 / Print Screen / 画图 等外部应用），补 BMP 头转 PNG
    if unsafe { IsClipboardFormatAvailable(CF_DIB.0 as u32) }.is_ok()
        && let Ok(handle) = unsafe { GetClipboardData(CF_DIB.0 as u32) }
        && let Some(dib) = unsafe { global_bytes(handle) }
        && let Some(png) = dib_to_png(&dib)
    {
        cap.image_dims = png_dims(&png);
        cap.image_png = Some(png);
        return Ok(Some(cap));
    }

    // 文本：CF_UNICODETEXT
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_ok()
        && let Ok(handle) = unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) }
        && let Some(bytes) = unsafe { global_bytes(handle) }
    {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_ne_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        if !units.is_empty() {
            cap.text = Some(String::from_utf16_lossy(&units));
            return Ok(Some(cap));
        }
    }

    Ok(None)
}

/// 写纯文本（UTF-16 + 结尾 NUL）。Windows 无 RTF 富文本快捷写入，rtf 忽略
pub fn write_text(text: &str, _rtf: Option<&[u8]>) -> Result<()> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes = unsafe { std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), units.len() * 2) };
    let _guard = Clipboard::open()?;
    unsafe {
        EmptyClipboard().map_err(|e| DomainError::Other(format!("清空剪贴板失败：{e}")))?;
        set_clipboard_bytes(CF_UNICODETEXT.0 as u32, bytes)
    }
}

/// 写图片：同时放 "PNG" 格式（Ramag 自身往返）与 CF_DIB（外部应用如 Paint / Word 识别）
pub fn write_image_png(png: &[u8]) -> Result<()> {
    let fmt = png_format();
    let _guard = Clipboard::open()?;
    unsafe {
        EmptyClipboard().map_err(|e| DomainError::Other(format!("清空剪贴板失败：{e}")))?;
        set_clipboard_bytes(fmt, png)?;
        // CF_DIB 供外部应用；转码失败不致命（PNG 格式已写）
        if let Some(dib) = png_to_dib(png) {
            set_clipboard_bytes(CF_DIB.0 as u32, &dib)?;
        }
        Ok(())
    }
}

/// 写文件列表（CF_HDROP）：DROPFILES 头 + 双 NUL 结尾的 UTF-16 路径串
pub fn write_files(paths: &[String]) -> Result<()> {
    // 构造 UTF-16 路径区：每条路径 NUL 结尾，整体再补一个 NUL
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
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

    let _guard = Clipboard::open()?;
    unsafe {
        EmptyClipboard().map_err(|e| DomainError::Other(format!("清空剪贴板失败：{e}")))?;
        set_clipboard_bytes(CF_HDROP.0 as u32, &blob)
    }
}

/// 构造 NUL 结尾的宽字符串，供 PCWSTR 传参
pub fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 供 app 模块复用：把宽串首指针包成 PCWSTR
pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR(buf.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::png_dims;

    #[test]
    fn png_header_dims() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&800u32.to_be_bytes());
        png.extend_from_slice(&600u32.to_be_bytes());
        assert_eq!(png_dims(&png), Some((800, 600)));
    }
}
