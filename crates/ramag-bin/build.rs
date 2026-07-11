//! 构建脚本：仅当目标平台为 Windows 时把 ramag.ico 嵌入 exe（任务栏 / 标题栏图标）。
//! 其它平台（macOS 走 .icns bundle）此脚本为空操作。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../scripts/icons/ramag.ico");
    // build.rs 的 cfg 反映宿主，判目标平台须用 CARGO_CFG_TARGET_OS 环境变量
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../scripts/icons/ramag.ico")
            .set("ProductName", "Ramag")
            .set("FileDescription", "Ramag Developer Toolbox")
            .set("InternalName", "ramag")
            .set("OriginalFilename", "ramag.exe")
            .set("CompanyName", "Ramag")
            .set("LegalCopyright", "Copyright (c) Ramag contributors");
        res.compile()?;
    }
    Ok(())
}
