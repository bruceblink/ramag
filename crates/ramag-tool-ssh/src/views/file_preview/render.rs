//! 远程文件编辑器渲染。

use gpui::{ClickEvent, Context, ParentElement, Render, Styled, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, input::Input, v_flex,
};
use ramag_domain::entities::{RemoteFileChunkPosition, format_bytes};

use super::super::file_preview_layout::remote_file_dialog_layout;
use super::RemoteFileEditor;

impl Render for RemoteFileEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let line_count = self.current_lines;
        let viewport = window.viewport_size();
        let layout = remote_file_dialog_layout(
            f32::from(viewport.width),
            f32::from(viewport.height),
            self.current_lines,
        );
        let editor_height = px(layout.editor_height);
        let read_only = self.is_read_only();
        let dirty = self.is_dirty();
        let windowed = self.windowed;
        let chunk_loading = self.chunk_loading;
        let auto_refresh_available = self.auto_refresh_available;
        let auto_refresh = self.auto_refresh;
        let has_previous = self.chunk_offset > 0;
        let has_next = self.chunk_end < self.total_bytes;
        let metadata = if windowed {
            format!(
                "{} · {}–{} · {} 行 · {} · 只读",
                format_bytes(self.total_bytes),
                format_bytes(self.chunk_offset),
                format_bytes(self.chunk_end),
                line_count,
                self.language,
            )
        } else {
            format!(
                "{} · {} 行 · {}{}",
                format_bytes(self.current_bytes as u64),
                line_count,
                self.language,
                if read_only { " · 只读" } else { "" }
            )
        };
        let editor_background = cx
            .theme()
            .highlight_theme
            .style
            .editor_background
            .unwrap_or_else(|| cx.theme().input_background());
        v_flex()
            .id("ssh-file-editor")
            .debug_selector(|| "ssh-file-editor".into())
            .w_full()
            .gap(px(8.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(metadata),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(6.0))
                            .when(auto_refresh_available, |actions| {
                                actions.child(
                                    ramag_ui::clickable_button("ssh-file-auto-refresh")
                                        .outline()
                                        .small()
                                        .icon(ramag_ui::icons::refresh_cw())
                                        .tooltip(if auto_refresh {
                                            "停止刷新"
                                        } else {
                                            "自动刷新"
                                        })
                                        .selected(auto_refresh)
                                        .disabled(self.saving || dirty)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.set_auto_refresh(
                                                    !this.auto_refresh,
                                                    window,
                                                    cx,
                                                );
                                            },
                                        )),
                                )
                            })
                            .when(windowed, |actions| {
                                actions
                                    .child(
                                        ramag_ui::clickable_button("ssh-file-previous")
                                            .outline()
                                            .small()
                                            .label("上段")
                                            .disabled(!has_previous || chunk_loading)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.load_chunk(
                                                        RemoteFileChunkPosition::Before(
                                                            this.chunk_offset,
                                                        ),
                                                        window,
                                                        cx,
                                                    );
                                                },
                                            )),
                                    )
                                    .child(
                                        ramag_ui::clickable_button("ssh-file-next")
                                            .outline()
                                            .small()
                                            .label("下段")
                                            .disabled(!has_next || chunk_loading)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.load_chunk(
                                                        RemoteFileChunkPosition::From(
                                                            this.chunk_end,
                                                        ),
                                                        window,
                                                        cx,
                                                    );
                                                },
                                            )),
                                    )
                                    .child(
                                        ramag_ui::clickable_button("ssh-file-tail")
                                            .outline()
                                            .small()
                                            .label("末尾")
                                            .loading(chunk_loading)
                                            .disabled(chunk_loading)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.load_chunk(
                                                        RemoteFileChunkPosition::Tail,
                                                        window,
                                                        cx,
                                                    );
                                                },
                                            )),
                                    )
                            })
                            .child(
                                ramag_ui::clickable_button("ssh-file-search")
                                    .outline()
                                    .small()
                                    .label("搜索")
                                    .disabled(chunk_loading)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.search(window, cx);
                                    })),
                            )
                            .when(dirty, |actions| {
                                actions.child(
                                    ramag_ui::clickable_button("ssh-file-discard")
                                        .ghost()
                                        .small()
                                        .label("放弃")
                                        .disabled(self.saving)
                                        .on_click(|_: &ClickEvent, window, cx| {
                                            window.close_dialog(cx);
                                        }),
                                )
                            })
                            .when(!read_only, |actions| {
                                actions.child(
                                    ramag_ui::clickable_button("ssh-file-save")
                                        .primary()
                                        .small()
                                        .label("保存")
                                        .loading(self.saving)
                                        .disabled(!dirty || self.saving)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.save(window, cx);
                                            },
                                        )),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(editor_height)
                    .overflow_hidden()
                    .child(
                        Input::new(&self.input)
                            .h_full()
                            .opacity(1.0)
                            .disabled(self.saving || chunk_loading),
                    )
                    // 编辑器仍可滚动，只遮住上游组件持续绘制的滚动条。
                    .child(
                        div()
                            .absolute()
                            .right_0()
                            .top_0()
                            .bottom_0()
                            .w(px(18.0))
                            .bg(editor_background),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(18.0))
                            .bg(editor_background),
                    ),
            )
    }
}
