//! 远程文本文件的有界查看、搜索与编辑。

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, Focusable as _, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState, Search},
    notification::Notification,
    v_flex,
};
use ramag_app::SshService;
use ramag_domain::entities::{
    MAX_REMOTE_FILE_PREVIEW_BYTES, RemoteEntry, RemoteEntryKind, RemoteFileChunkPosition,
    SshProfile, SshProfileId, format_bytes,
};

use super::SshView;
use super::file_chunk::{RemoteFileText, decode_remote_file_chunk, text_line_count};
use super::model::{Notice, ViewMode};

impl SshView {
    pub(super) fn preview_remote_file(
        &mut self,
        workspace_id: SshProfileId,
        entry: RemoteEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entry.kind != RemoteEntryKind::File {
            self.notice = Some(Notice::error("仅支持普通文件"));
            cx.notify();
            return;
        }
        let Some(workspace) = self.workspace_mut(&workspace_id) else {
            return;
        };
        if workspace.file_preview_loading {
            return;
        }
        if !workspace
            .entries
            .iter()
            .any(|current| current.path == entry.path && current.kind == RemoteEntryKind::File)
        {
            self.notice = Some(Notice::error("文件已变化，请刷新"));
            cx.notify();
            return;
        }
        workspace.file_preview_loading = true;
        workspace.file_preview_generation = workspace.file_preview_generation.wrapping_add(1);
        let generation = workspace.file_preview_generation;
        let profile = workspace.profile.clone();
        let path = entry.path.clone();
        let service = self.service.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, async_cx| {
            let position = RemoteFileChunkPosition::Tail;
            let result = service.read_file_chunk(&profile, &path, position).await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                let is_current = {
                    let Some(workspace) = this.workspace_mut(&workspace_id) else {
                        return;
                    };
                    if workspace.file_preview_generation != generation {
                        return;
                    }
                    workspace.file_preview_loading = false;
                    workspace.entries.iter().any(|current| {
                        current.path == path && current.kind == RemoteEntryKind::File
                    })
                } && this.view_mode == ViewMode::Workspace
                    && this.active_workspace_id.as_ref() == Some(&workspace_id);
                if !is_current {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(chunk) => match decode_remote_file_chunk(chunk, position) {
                        Ok(preview) => open_remote_file_editor(
                            this.service.clone(),
                            cx.entity(),
                            profile,
                            entry,
                            preview,
                            window,
                            cx,
                        ),
                        Err(error) => this.notice = Some(Notice::error(error)),
                    },
                    Err(error) => this.notice = Some(Notice::error(format!("查看失败：{error}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

struct RemoteFileEditor {
    service: Arc<SshService>,
    owner: Entity<SshView>,
    profile: SshProfile,
    entry: RemoteEntry,
    input: Entity<InputState>,
    original_text: String,
    current_bytes: usize,
    current_lines: usize,
    dirty: bool,
    total_bytes: u64,
    chunk_offset: u64,
    chunk_end: u64,
    windowed: bool,
    language: &'static str,
    chunk_loading: bool,
    chunk_generation: u64,
    saving: bool,
    save_generation: u64,
    _subscription: Subscription,
}

impl RemoteFileEditor {
    fn new(
        service: Arc<SshService>,
        owner: Entity<SshView>,
        profile: SshProfile,
        entry: RemoteEntry,
        preview: RemoteFileText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let language = super::file_syntax::language_for_remote_file(&entry.path, &preview.text);
        let windowed = preview.is_windowed();
        let original_text = preview.text;
        let current_bytes = original_text.len();
        let current_lines = text_line_count(&original_text);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(true)
                .soft_wrap(false)
                .indent_guides(false)
                .folding(false)
                .default_value(original_text.clone())
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = this.input.read(cx).value();
                    if this.is_read_only() && value.as_ref() != this.original_text.as_str() {
                        let original = this.original_text.clone();
                        this.input.update(cx, |input, cx| {
                            input.set_value(original, window, cx);
                        });
                    } else if !this.is_read_only() {
                        this.current_bytes = value.len();
                        this.current_lines = text_line_count(&value);
                        this.dirty = value.as_ref() != this.original_text.as_str();
                    }
                    cx.notify();
                }
            });
        Self {
            service,
            owner,
            profile,
            entry,
            input,
            original_text,
            current_bytes,
            current_lines,
            dirty: false,
            total_bytes: preview.total_bytes,
            chunk_offset: preview.offset,
            chunk_end: preview.end_offset,
            windowed,
            language,
            chunk_loading: false,
            chunk_generation: 0,
            saving: false,
            save_generation: 0,
            _subscription: subscription,
        }
    }

    fn is_read_only(&self) -> bool {
        self.profile.production || self.windowed
    }

    fn is_dirty(&self) -> bool {
        !self.is_read_only() && self.dirty
    }

    fn search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.read(cx).focus_handle(cx).focus(window, cx);
        window.dispatch_action(Box::new(Search), cx);
    }

    fn load_chunk(
        &mut self,
        position: RemoteFileChunkPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chunk_loading || self.saving || self.is_dirty() {
            return;
        }
        self.chunk_loading = true;
        self.chunk_generation = self.chunk_generation.wrapping_add(1);
        let generation = self.chunk_generation;
        let service = self.service.clone();
        let profile = self.profile.clone();
        let path = self.entry.path.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, async_cx| {
            let result = service.read_file_chunk(&profile, &path, position).await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                if this.chunk_generation != generation {
                    return;
                }
                this.chunk_loading = false;
                match result {
                    Ok(chunk) => match decode_remote_file_chunk(chunk, position) {
                        Ok(preview) => this.replace_chunk(preview, window, cx),
                        Err(error) => window.push_notification(Notification::error(error), cx),
                    },
                    Err(error) => window
                        .push_notification(Notification::error(format!("读取失败：{error}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn replace_chunk(
        &mut self,
        preview: RemoteFileText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let windowed = preview.is_windowed();
        let text = preview.text;
        self.current_bytes = text.len();
        self.current_lines = text_line_count(&text);
        self.original_text = text.clone();
        self.total_bytes = preview.total_bytes;
        self.chunk_offset = preview.offset;
        self.chunk_end = preview.end_offset;
        self.windowed = windowed;
        self.dirty = false;
        self.input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.is_read_only() || !self.is_dirty() {
            return;
        }
        let contents = self.input.read(cx).value().to_string();
        if contents.len() > MAX_REMOTE_FILE_PREVIEW_BYTES {
            window.push_notification(
                Notification::error(format!(
                    "不能超过 {} MiB",
                    MAX_REMOTE_FILE_PREVIEW_BYTES / 1024 / 1024
                )),
                cx,
            );
            return;
        }
        self.saving = true;
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let service = self.service.clone();
        let profile = self.profile.clone();
        let path = self.entry.path.clone();
        let expected = self.original_text.as_bytes().to_vec();
        let bytes = contents.as_bytes().to_vec();
        cx.notify();
        cx.spawn_in(window, async move |this, async_cx| {
            let result = service.save_file(&profile, &path, &expected, &bytes).await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                if this.save_generation != generation {
                    return;
                }
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.original_text = contents;
                        this.dirty =
                            this.input.read(cx).value().as_ref() != this.original_text.as_str();
                        this.total_bytes = bytes.len() as u64;
                        this.chunk_offset = 0;
                        this.chunk_end = bytes.len() as u64;
                        this.windowed = false;
                        this.owner.update(cx, |owner, cx| {
                            owner.refresh_directory(profile.id.clone(), None, cx);
                        });
                        window
                            .push_notification(Notification::success("已保存").autohide(true), cx);
                    }
                    Err(error) => window
                        .push_notification(Notification::error(format!("保存失败：{error}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn request_close(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            window.push_notification(Notification::warning("正在保存"), cx);
        } else if self.is_dirty() {
            window.push_notification(Notification::warning("修改未保存"), cx);
        } else {
            window.close_dialog(cx);
        }
    }
}

impl Render for RemoteFileEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let line_count = self.current_lines;
        let visible_rows = line_count.max(1).saturating_add(3).min(32);
        let editor_height = px(visible_rows as f32 * 22.0 + 12.0);
        let read_only = self.is_read_only();
        let dirty = self.is_dirty();
        let windowed = self.windowed;
        let chunk_loading = self.chunk_loading;
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
                    // 编辑器仍可用鼠标滚轮滚动，只隐藏持续占位的滚动条。
                    .child(
                        div()
                            .absolute()
                            .right_0()
                            .top_0()
                            .bottom_0()
                            .w(px(10.0))
                            .bg(editor_background),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(10.0))
                            .bg(editor_background),
                    ),
            )
    }
}

fn open_remote_file_editor(
    service: Arc<SshService>,
    owner: Entity<SshView>,
    profile: SshProfile,
    entry: RemoteEntry,
    preview: RemoteFileText,
    window: &mut Window,
    cx: &mut Context<SshView>,
) {
    let title = bounded_preview_title(&entry.path);
    let editor =
        cx.new(|cx| RemoteFileEditor::new(service, owner, profile, entry, preview, window, cx));
    let dialog_width = px((f32::from(window.viewport_size().width) - 64.0).clamp(320.0, 1600.0));
    window.open_dialog(cx, move |dialog, _, _| {
        let title_editor = editor.clone();
        let content_editor = editor.clone();
        dialog
            .title(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(title.clone())
                    .child(
                        ramag_ui::clickable_button("ssh-file-editor-close")
                            .ghost()
                            .xsmall()
                            .icon(gpui_component::IconName::Close)
                            .on_click(move |_: &ClickEvent, window, app| {
                                title_editor.update(app, |this, cx| {
                                    this.request_close(window, cx);
                                });
                            }),
                    ),
            )
            .close_button(false)
            .overlay_closable(false)
            .keyboard(false)
            .margin_top(px(36.0))
            .w(dialog_width)
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
