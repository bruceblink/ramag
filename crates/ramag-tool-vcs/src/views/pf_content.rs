//! Project Files 主区：原生 Code Editor，支持编辑、增量语法解析和完整长行显示。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, input::Input, v_flex};

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
        let mut body = v_flex().size_full().min_h_0().child(header_bar(
            &snapshot.path,
            self.pf_editor_line_count,
            self.pf_editor_dirty,
            self.saving_file_content,
            editable,
            muted_fg,
            fg,
            cx,
        ));
        if snapshot.truncated {
            body = body.child(truncated_banner(muted_fg));
        }

        body.child(div().flex_1().min_h_0().min_w_0().child(if editor_ready {
            Input::new(&self.pf_editor)
                .h_full()
                .bordered(false)
                .focus_bordered(false)
                .disabled(!editable || self.saving_file_content)
                .into_any_element()
        } else {
            placeholder("准备编辑器…", muted_fg)
        }))
        .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn header_bar(
    path: &str,
    line_count: usize,
    dirty: bool,
    saving: bool,
    editable: bool,
    muted_fg: gpui::Hsla,
    fg: gpui::Hsla,
    cx: &mut Context<VcsView>,
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
        .when(dirty, |row| {
            row.child(div().text_xs().text_color(fg).child("未保存"))
        })
        .child(
            ramag_ui::clickable_button("vcs-pf-save")
                .outline()
                .xsmall()
                .label(if saving { "保存中…" } else { "保存" })
                .tooltip(format!(
                    "保存当前文件（{}）",
                    ramag_ui::platform::primary_shortcut("S")
                ))
                .disabled(!editable || !dirty || saving)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.save_project_file(cx);
                })),
        )
        .into_any_element()
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
        .child("文件较大，仅预览前 4 MiB；为避免破坏未加载内容，已禁用编辑和保存")
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
