//! 共享 Markdown 预览适配：保留虚拟滚动，并隐藏行内代码的 accent 底色。

use std::path::{Component, Path, PathBuf};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, IntoElement, LayoutId, Pixels,
    SharedString, Window,
};
use gpui_component::{Theme, text};
use url::Url;

/// 创建长文 Markdown 预览。
///
/// `gpui-component` 的 TextView 会把行内代码直接绘制成主题 accent 背景；预览中仅在它的
/// 布局、预绘制和绘制期间临时将 accent 置为透明，链接颜色、代码语义和虚拟滚动均保留。
pub fn markdown_preview(source: impl Into<gpui::SharedString>) -> impl IntoElement {
    MarkdownPreviewElement {
        child: text::markdown(source).scrollable(true).into_any_element(),
    }
}

/// 创建带仓库文件路径上下文的 Markdown 预览。
///
/// `gpui-component` 的 Markdown 解析器会把图片和链接原样交给 GPUI。VCS 文件内容中的
/// `docs/a.png`、`docs/a.md` 需要先相对于当前文档解析，否则图片会静默为空，链接会被系统
/// 当作不完整 URL 处理并返回 macOS `-50`。
pub fn markdown_preview_at_path(
    source: impl Into<SharedString>,
    document_path: impl AsRef<Path>,
) -> impl IntoElement {
    let source = resolve_local_references(source.into().as_ref(), document_path.as_ref());
    markdown_preview(source)
}

#[derive(Clone, Copy)]
enum ReferenceKind {
    Link,
    Image,
}

fn resolve_local_references(source: &str, document_path: &Path) -> String {
    let Some(base_dir) = document_path.parent() else {
        return source.to_owned();
    };

    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    while cursor < source.len() {
        if source[cursor..].starts_with("](") {
            output.push_str("](");
            cursor += 2;
            if let Some((target_start, target_end, replacement)) =
                rewrite_markdown_target(source, cursor, base_dir)
            {
                output.push_str(&source[cursor..target_start]);
                output.push_str(&replacement);
                cursor = target_end;
            }
            continue;
        }

        if let Some((attribute, target_start, kind)) = html_reference_attribute(source, cursor) {
            output.push_str(attribute);
            if let Some(target_end) = quoted_attribute_end(source, target_start) {
                let target = &source[target_start..target_end];
                let replacement =
                    resolve_reference(target, base_dir, kind).unwrap_or_else(|| target.to_owned());
                output.push_str(&replacement);
                cursor = target_end;
                continue;
            }
            cursor = target_start;
            continue;
        }

        let Some(ch) = source[cursor..].chars().next() else {
            break;
        };
        output.push(ch);
        cursor += ch.len_utf8();
    }
    output
}

fn rewrite_markdown_target(
    source: &str,
    start: usize,
    base_dir: &Path,
) -> Option<(usize, usize, String)> {
    let mut target_start = start;
    while source[target_start..]
        .chars()
        .next()
        .is_some_and(|ch| ch.is_whitespace())
    {
        target_start += source[target_start..].chars().next()?.len_utf8();
    }

    let (value_start, value_end) = if source[target_start..].starts_with('<') {
        let value_start = target_start + 1;
        let value_end = source[value_start..].find('>')?.checked_add(value_start)?;
        (value_start, value_end)
    } else {
        let mut end = target_start;
        let mut parentheses = 0usize;
        while end < source.len() {
            let ch = source[end..].chars().next()?;
            match ch {
                '\\' => {
                    end += ch.len_utf8();
                    if end < source.len() {
                        end += source[end..].chars().next()?.len_utf8();
                    }
                }
                '(' => {
                    parentheses += 1;
                    end += ch.len_utf8();
                }
                ')' if parentheses == 0 => break,
                ')' => {
                    parentheses -= 1;
                    end += ch.len_utf8();
                }
                c if c.is_whitespace() && parentheses == 0 => break,
                _ => end += ch.len_utf8(),
            }
        }
        (target_start, end)
    };

    let target = &source[value_start..value_end];
    let kind = if source[..start.saturating_sub(2)]
        .rsplit_once('[')
        .is_some_and(|(prefix, _)| prefix.ends_with('!'))
    {
        ReferenceKind::Image
    } else {
        ReferenceKind::Link
    };
    let replacement = resolve_reference(target, base_dir, kind)?;
    Some((value_start, value_end, replacement))
}

fn html_reference_attribute(source: &str, cursor: usize) -> Option<(&str, usize, ReferenceKind)> {
    [
        ("src=\"", ReferenceKind::Image),
        ("src='", ReferenceKind::Image),
        ("href=\"", ReferenceKind::Link),
        ("href='", ReferenceKind::Link),
    ]
    .iter()
    .find_map(|(attribute, kind)| {
        source[cursor..]
            .starts_with(attribute)
            .then(|| (*attribute, cursor + attribute.len(), *kind))
    })
}

fn quoted_attribute_end(source: &str, start: usize) -> Option<usize> {
    let quote = source.as_bytes().get(start.wrapping_sub(1)).copied()?;
    source.as_bytes()[start..]
        .iter()
        .position(|byte| *byte == quote)
        .map(|offset| start + offset)
}

fn resolve_reference(target: &str, base_dir: &Path, kind: ReferenceKind) -> Option<String> {
    let target = target.trim();
    if target.is_empty()
        || target.starts_with('#')
        || target.starts_with("//")
        || target.starts_with("data:")
        || target.contains("://")
        || target.starts_with("mailto:")
    {
        return None;
    }

    let suffix_start = target.find(['?', '#']).unwrap_or(target.len());
    let path_part = &target[..suffix_start];
    if path_part.is_empty() {
        return None;
    }
    let suffix = &target[suffix_start..];
    let path = normalize_path(if Path::new(path_part).is_absolute() {
        PathBuf::from(path_part)
    } else {
        base_dir.join(path_part)
    });

    match kind {
        ReferenceKind::Image => Some(path.to_string_lossy().into_owned()),
        ReferenceKind::Link => {
            let mut url = Url::from_file_path(path).ok()?;
            let (query, fragment) = suffix
                .split_once('#')
                .map_or((suffix.strip_prefix('?'), None), |(before, fragment)| {
                    (before.strip_prefix('?'), Some(fragment))
                });
            url.set_query(query);
            url.set_fragment(fragment);
            Some(url.into())
        }
    }
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

struct MarkdownPreviewElement {
    child: AnyElement,
}

impl IntoElement for MarkdownPreviewElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MarkdownPreviewElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id = with_transparent_accent(cx, |cx| self.child.request_layout(window, cx));
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        with_transparent_accent(cx, |cx| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        with_transparent_accent(cx, |cx| self.child.paint(window, cx));
    }
}

/// TextView 的 Entity 可能在布局或预绘制阶段重建子树；三个阶段都临时覆盖才可稳定影响
/// gpui-component 对行内代码背景的硬编码取色。
fn with_transparent_accent<T>(cx: &mut App, render: impl FnOnce(&mut App) -> T) -> T {
    let original_accent = Theme::global(cx).accent;
    let transparent = Theme::global(cx).transparent;
    Theme::global_mut(cx).accent = transparent;
    let result = render(cx);
    Theme::global_mut(cx).accent = original_accent;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn inline_code_background_scope_is_transparent_and_restored(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let original_accent = Theme::global(cx).accent;
            let scoped_accent = with_transparent_accent(cx, |cx| Theme::global(cx).accent);

            assert_eq!(scoped_accent.a, 0.0);
            assert_eq!(Theme::global(cx).accent, original_accent);
        });
    }

    #[test]
    fn local_markdown_references_use_the_document_directory() {
        let source = r#"[构建](docs/desktop-release.md#本地-linux-打包)
![二维码](docs/community/group-qr.png)
<img src="docs/community/personal-qr.png">
<a href='docs/development-guide.md'>架构</a>
[官网](https://example.com/docs)"#;

        let resolved = resolve_local_references(source, Path::new("/repo/README.md"));

        assert!(resolved.contains(
            "file:///repo/docs/desktop-release.md#%E6%9C%AC%E5%9C%B0-linux-%E6%89%93%E5%8C%85"
        ));
        assert!(resolved.contains("![二维码](/repo/docs/community/group-qr.png)"));
        assert!(resolved.contains("<img src=\"/repo/docs/community/personal-qr.png\">"));
        assert!(resolved.contains("file:///repo/docs/development-guide.md"));
        assert!(resolved.contains("[官网](https://example.com/docs)"));
    }
}
