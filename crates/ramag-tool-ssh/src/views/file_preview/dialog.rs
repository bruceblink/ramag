use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, ParentElement, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex};
use ramag_app::SshService;
use ramag_domain::entities::SshProfile;

use super::super::{SshView, file_preview_layout::remote_file_dialog_layout};
use super::{RemoteFileEditor, RemoteFileEditorInput};

pub(super) fn open_remote_file_editor(
    service: Arc<SshService>,
    owner: Entity<SshView>,
    profile: SshProfile,
    editor_input: RemoteFileEditorInput,
    window: &mut Window,
    cx: &mut Context<SshView>,
) {
    let title = bounded_preview_title(&editor_input.entry.path);
    let editor =
        cx.new(|cx| RemoteFileEditor::new(service, owner, profile, editor_input, window, cx));
    if editor.read(cx).auto_refresh {
        editor.update(cx, |this, cx| this.spawn_auto_refresh(window, cx));
    }
    window.open_dialog(cx, move |dialog, window, app| {
        let viewport = window.viewport_size();
        let layout = remote_file_dialog_layout(
            f32::from(viewport.width),
            f32::from(viewport.height),
            editor.read(app).current_lines,
        );
        let title_editor = editor.clone();
        let content_editor = editor.clone();
        dialog
            .title(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(title.clone()),
                    )
                    .child(
                        div().flex_none().child(
                            ramag_ui::clickable_button("ssh-file-editor-close")
                                .ghost()
                                .xsmall()
                                .icon(gpui_component::IconName::Close)
                                .tooltip("关闭")
                                .on_click(move |_: &ClickEvent, window, app| {
                                    title_editor.update(app, |this, cx| {
                                        this.request_close(window, cx);
                                    });
                                }),
                        ),
                    ),
            )
            .close_button(false)
            .overlay_closable(false)
            .keyboard(false)
            .margin_top(px(layout.margin_top))
            .w(px(layout.width))
            .p(px(14.0))
            .content(move |content, _, _| content.child(content_editor.clone()))
    });
}

fn bounded_preview_title(path: &str) -> SharedString {
    const MAX_TITLE_CHARS: usize = 120;
    let mut chars = path.chars();
    let mut title = chars.by_ref().take(MAX_TITLE_CHARS).collect::<String>();
    if chars.next().is_some() {
        title.push('…');
    }
    title.into()
}

pub(super) fn remote_file_read_only_reason(
    production: bool,
    windowed: bool,
    auto_refresh: bool,
) -> Option<&'static str> {
    if production {
        Some("生产模式")
    } else if windowed {
        Some("分段预览")
    } else if auto_refresh {
        Some("自动刷新")
    } else {
        None
    }
}
