//! 构建脚本：仅当目标平台为 Windows 时把 ramag.ico 嵌入 exe（任务栏 / 标题栏图标）。
//! 其它平台（macOS 走 .icns bundle）此脚本为空操作。

fn main() {
    // build.rs 的 cfg 反映宿主，判目标平台须用 CARGO_CFG_TARGET_OS 环境变量
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../scripts/icons/ramag.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=嵌入 Windows 图标失败: {e}");
        }
    }
}
