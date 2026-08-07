//! 跨平台剪贴板接口；系统 UI 操作必须在 GPUI 前台执行器调用。

use std::sync::Arc;

use crate::entities::{CapturedClip, ClipSource};
use crate::error::Result;

pub trait ClipboardDriver: Send + Sync {
    /// 系统剪贴板修改计数。
    fn change_count(&self) -> i64;

    /// 本应用最近一次写回产生的修改计数。
    fn own_change_count(&self) -> i64;

    /// 空剪贴板或无可识别类型时返回 `None`。
    fn read(&self) -> Result<Option<CapturedClip>>;

    /// 写文本回剪贴板（可附带 RTF 富文本表示）
    fn write_text(&self, text: &str, rtf: Option<&[u8]>) -> Result<()>;

    fn write_image_png(&self, png: &[u8]) -> Result<()>;

    fn write_files(&self, paths: &[String]) -> Result<()>;

    /// 采集来源应用标注。实现可返回比「当前前台应用」更精确的来源
    /// （如 Windows 返回最近一次 read 时的剪贴板 owner 进程）
    fn frontmost_app(&self) -> Option<ClipSource>;

    /// 当前前台应用的短期激活标识。默认复用应用 id；平台可覆盖为更精确的窗口标识。
    fn activation_target(&self) -> Option<String> {
        self.frontmost_app().map(|source| source.bundle_id)
    }

    /// 获取应用图标；实现负责缓存。
    fn app_icon_png(&self, bundle_id: &str) -> Option<Arc<Vec<u8>>>;

    /// 字节落盘到媒体缓存（key 形如 `{hash}.img` / `{hash}.thumb`，同名去重），返回路径。
    /// 加密由上层 service 负责，此处只写原始字节
    fn persist_media(&self, key: &str, bytes: &[u8]) -> Result<String>;

    /// 读取媒体密文，解密由上层服务负责。
    fn read_media(&self, path: &str) -> Result<Vec<u8>>;

    fn list_media(&self) -> Result<Vec<String>>;

    /// 删除媒体文件，文件不存在时仍成功。
    fn remove_media(&self, path: &str) -> Result<()>;

    /// 流式清空受管媒体目录，不把全部路径先收集到内存。
    fn clear_media(&self) -> Result<()>;

    /// 自动粘贴所需的系统权限是否已授予；prompt=true 时弹系统授权引导
    fn accessibility_trusted(&self, prompt: bool) -> bool;

    /// 激活指定目标（None 跳过激活）并模拟平台粘贴快捷键
    fn paste_to_app(&self, activation_target: Option<&str>) -> Result<()>;

    fn open_url(&self, url: &str) -> Result<()>;

    fn reveal_in_file_manager(&self, paths: &[String]) -> Result<()>;

    fn paths_exist(&self, paths: &[String]) -> bool;
}
