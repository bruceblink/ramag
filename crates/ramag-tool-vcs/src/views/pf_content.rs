//! Project Files 主区：原生 Code Editor，支持自动保存、增量语法解析和完整长行显示。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _, button::ButtonVariants as _, clipboard::Clipboard, h_flex,
    input::Input, v_flex,
};

use super::vcs_view::VcsView;

impl VcsView {
    pub(super) fn render_pf_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;

        if self.loading_file_content {
            return placeholder("加载中…", muted_fg);
        }

        let Some(snapshot) = self.current_file_content.as_ref() else {
            return placeholder("在左侧选择文件以查看内容", muted_fg);
        };
        if let Some(error) = &snapshot.error {
            return placeholder(error.clone(), gpui::hsla(0.0, 0.65, 0.55, 1.0));
        }
        if snapshot.binary {
            return placeholder("（二进制文件，未渲染内容）", muted_fg);
        }

        let editor_ready = self.pf_editor_loaded_path.as_deref() == Some(snapshot.path.as_str());
        let editable = !snapshot.truncated;
        let markdown = is_markdown_path(&snapshot.path);
        let show_source = !markdown || self.pf_show_source;
        let source_toggle = markdown.then(|| {
            ramag_ui::clickable_button("vcs-pf-markdown-source")
                .ghost()
                .xsmall()
                .label(if show_source { "预览" } else { "原文" })
                .tooltip(if show_source {
                    "渲染 Markdown"
                } else {
                    "查看并编辑原文"
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_markdown_source_visible(!show_source, cx);
                }))
                .into_any_element()
        });
        let snapshot_text = snapshot.text.clone();
        let editor_for_copy = self.pf_editor.clone();
        let markdown_document_path = self
            .repo
            .as_ref()
            .map(|repo| std::path::Path::new(&repo.path).join(&snapshot.path));
        let copy_button = Clipboard::new("vcs-pf-copy")
            .tooltip("复制")
            .value_fn(move |_, app| {
                if editor_ready {
                    editor_for_copy.read(app).value()
                } else {
                    snapshot_text.as_str().to_string().into()
                }
            })
            .into_any_element();
        let actions = h_flex()
            .gap(px(4.0))
            .children(source_toggle)
            .child(copy_button)
            .into_any_element();
        let mut body = v_flex().size_full().min_h_0().child(header_bar(
            &snapshot.path,
            self.pf_editor_line_count,
            muted_fg,
            fg,
            actions,
        ));
        if snapshot.truncated {
            body = body.child(truncated_banner(muted_fg));
        }

        let content = if markdown && !show_source {
            div()
                .id("vcs-markdown-preview")
                .size_full()
                .p(px(20.0))
                // 让 TextView 自己使用 ListState 滚动，只布局可视区域，避免长文滚动时
                // 每一帧都重新处理整篇 Markdown。预览区通过顶部复制按钮复制全文。
                .child(ramag_ui::markdown_preview_at_path(
                    snapshot.text.as_str().to_string(),
                    markdown_document_path
                        .as_deref()
                        .unwrap_or_else(|| std::path::Path::new(&snapshot.path)),
                ))
                .into_any_element()
        } else if editor_ready {
            Input::new(&self.pf_editor)
                .h_full()
                .bordered(false)
                .focus_bordered(false)
                .disabled(!editable)
                .into_any_element()
        } else {
            placeholder("准备编辑器…", muted_fg)
        };
        body.child(div().flex_1().min_h_0().min_w_0().child(content))
            .into_any_element()
    }

    fn set_markdown_source_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.pf_show_source == visible {
            return;
        }
        if !visible {
            self.capture_active_project_draft(cx);
        }
        self.pf_show_source = visible;
        cx.notify();
    }
}

fn header_bar(
    path: &str,
    line_count: usize,
    muted_fg: gpui::Hsla,
    fg: gpui::Hsla,
    action: AnyElement,
) -> AnyElement {
    h_flex()
        .w_full()
        .flex_none()
        .px(px(12.0))
        .py(px(6.0))
        .gap(px(8.0))
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(path.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted_fg)
                .child(format!("{line_count} 行")),
        )
        .child(action)
        .into_any_element()
}

fn is_markdown_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
        })
}

fn truncated_banner(muted_fg: gpui::Hsla) -> AnyElement {
    let mut bg = gpui::hsla(40.0 / 360.0, 0.7, 0.55, 1.0);
    bg.a = 0.10;
    div()
        .w_full()
        .px(px(12.0))
        .py(px(6.0))
        .bg(bg)
        .text_xs()
        .text_color(muted_fg)
        .child("文件较大，仅预览前 4 MiB；为避免破坏未加载内容，已禁用编辑")
        .into_any_element()
}

fn placeholder(text: impl Into<SharedString>, color: gpui::Hsla) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(div().text_sm().text_color(color).child(text.into()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::is_markdown_path;

    #[test]
    fn markdown_extensions_are_case_insensitive() {
        assert!(is_markdown_path("README.md"));
        assert!(is_markdown_path("docs/guide.MARKDOWN"));
        assert!(!is_markdown_path("notes.txt"));
    }
}
