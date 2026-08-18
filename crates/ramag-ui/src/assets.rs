//! AssetSource：优先 ramag-ui 内嵌 svg（assets/icons），未命中回退 gpui_component_assets

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// 编译期内嵌 svg
#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
struct LocalAssets;

#[derive(Default, Clone, Copy)]
pub struct RamagAssets;

impl AssetSource for RamagAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some(file) = LocalAssets::get(path) {
            return Ok(Some(file.data));
        }
        // Markdown 预览会把仓库内相对图片解析为绝对路径；GPUI 的 `img` 将其作为
        // Embedded 资源交给 AssetSource，因此这里补充本地文件回退。
        if std::path::Path::new(path).is_absolute()
            && let Ok(data) = std::fs::read(path)
        {
            return Ok(Some(Cow::Owned(data)));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = LocalAssets::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();
        if let Ok(upstream) = gpui_component_assets::Assets.list(path) {
            out.extend(upstream);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_absolute_files_for_markdown_images() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/community/group-qr.png");
        let loaded = RamagAssets
            .load(path.to_string_lossy().as_ref())
            .ok()
            .flatten();

        assert!(loaded.is_some());
    }
}
