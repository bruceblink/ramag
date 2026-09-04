//! 远程文本文件的有界查看、搜索与编辑。

mod dialog;
mod follow;
mod render;

use std::{sync::Arc, time::Duration};

use gpui::{AppContext as _, Context, Entity, Focusable as _, Subscription, Window};
use gpui_component::{
    WindowExt as _,
    input::{InputEvent, InputState, Position, Search},
    notification::Notification,
};
use ramag_app::SshService;
use ramag_domain::entities::{
    MAX_REMOTE_FILE_PREVIEW_BYTES, RemoteEntry, RemoteEntryKind, RemoteFileChunkPosition,
    SshProfile, SshProfileId,
};
use tracing::error;

use super::SshView;
use super::file_chunk::{
    RemoteFileText, decode_remote_file_chunk, merge_remote_file_tail, text_line_count,
};
use super::model::{Notice, ViewMode};

use dialog::{open_remote_file_editor, remote_file_read_only_reason};

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
            if let Err(error) = &result {
                error!(
                    operation = "ssh_file_preview",
                    profile_id = %profile.id,
                    path = %path,
                    position = ?position,
                    error = %error,
                    "read remote file preview failed"
                );
            }
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
                            RemoteFileEditorInput { entry, preview },
                            window,
                            cx,
                        ),
                        Err(error) => {
                            error!(
                                operation = "ssh_file_preview_decode",
                                profile_id = %profile.id,
                                path = %path,
                                position = ?position,
                                error = %error,
                                "decode remote file preview failed"
                            );
                            this.notice = Some(Notice::error(error));
                        }
                    },
                    Err(error) => this.notice = Some(Notice::error(format!("查看失败：{error}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

struct RemoteFileEditorInput {
    entry: RemoteEntry,
    preview: RemoteFileText,
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
    auto_refresh_available: bool,
    auto_refresh: bool,
    auto_refresh_loading: bool,
    auto_refresh_failed: bool,
    auto_refresh_generation: u64,
    saving: bool,
    save_generation: u64,
    _subscription: Subscription,
}

impl RemoteFileEditor {
    fn new(
        service: Arc<SshService>,
        owner: Entity<SshView>,
        profile: SshProfile,
        editor_input: RemoteFileEditorInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let RemoteFileEditorInput { entry, preview } = editor_input;
        let language = super::file_syntax::language_for_remote_file(&entry.path, &preview.text);
        let windowed = preview.is_windowed();
        let original_text = preview.text;
        let current_bytes = original_text.len();
        let current_lines = text_line_count(&original_text);
        let auto_refresh_available = follow::supports_auto_refresh(&entry.path, windowed);
        let auto_refresh = follow::enables_auto_refresh(&entry.path);
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
            auto_refresh_available,
            auto_refresh,
            auto_refresh_loading: false,
            auto_refresh_failed: false,
            auto_refresh_generation: 0,
            saving: false,
            save_generation: 0,
            _subscription: subscription,
        }
    }

    fn is_read_only(&self) -> bool {
        self.read_only_reason().is_some()
    }

    fn read_only_reason(&self) -> Option<&'static str> {
        remote_file_read_only_reason(self.profile.production, self.windowed, self.auto_refresh)
    }

    fn is_dirty(&self) -> bool {
        !self.is_read_only() && self.dirty
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.disable_auto_refresh();
        self.input.read(cx).focus_handle(cx).focus(window, cx);
        window.dispatch_action(Box::new(Search), cx);
        cx.notify();
    }

    fn set_auto_refresh(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        if enabled == self.auto_refresh
            || (enabled && (!self.auto_refresh_available || self.is_dirty() || self.saving))
        {
            return;
        }
        if !enabled {
            self.disable_auto_refresh();
            cx.notify();
            return;
        }

        self.auto_refresh = true;
        self.auto_refresh_failed = false;
        self.auto_refresh_generation = self.auto_refresh_generation.wrapping_add(1);
        self.refresh_tail(window, cx);
        self.spawn_auto_refresh(window, cx);
        cx.notify();
    }

    fn disable_auto_refresh(&mut self) {
        if !self.auto_refresh && !self.auto_refresh_loading {
            return;
        }
        self.auto_refresh = false;
        self.auto_refresh_loading = false;
        self.auto_refresh_generation = self.auto_refresh_generation.wrapping_add(1);
        self.auto_refresh_available =
            follow::supports_auto_refresh(&self.entry.path, self.windowed);
    }

    fn spawn_auto_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.auto_refresh_generation;
        cx.spawn_in(window, async move |this, async_cx| {
            loop {
                async_cx
                    .background_executor()
                    .timer(Duration::from_secs(2))
                    .await;
                let refresh = this.update_in(async_cx, |this, window, cx| {
                    if !this.auto_refresh || this.auto_refresh_generation != generation {
                        return false;
                    }
                    this.refresh_tail(window, cx);
                    true
                });
                if !matches!(refresh, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn refresh_tail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.auto_refresh
            || self.auto_refresh_loading
            || self.chunk_loading
            || self.saving
            || self.is_dirty()
        {
            return;
        }
        self.auto_refresh_loading = true;
        let generation = self.auto_refresh_generation;
        let service = self.service.clone();
        let profile = self.profile.clone();
        let path = self.entry.path.clone();
        let known_end = self.chunk_end;
        let known_total = self.total_bytes;
        cx.spawn_in(window, async move |this, async_cx| {
            let result =
                follow::read_tail_update(&service, &profile, &path, known_end, known_total).await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                if !this.auto_refresh || this.auto_refresh_generation != generation {
                    return;
                }
                this.auto_refresh_loading = false;
                match result {
                    Ok(follow::TailUpdate::Unchanged) => {
                        this.auto_refresh_failed = false;
                    }
                    Ok(follow::TailUpdate::Append(appended)) => {
                        let current = RemoteFileText {
                            text: this.original_text.clone(),
                            total_bytes: this.total_bytes,
                            offset: this.chunk_offset,
                            end_offset: this.chunk_end,
                        };
                        match merge_remote_file_tail(&current, appended) {
                            Ok(preview) => {
                                this.auto_refresh_failed = false;
                                this.replace_chunk(preview, true, window, cx);
                            }
                            Err(error) => this.report_auto_refresh_error(error, window, cx),
                        }
                    }
                    Ok(follow::TailUpdate::Replace(preview)) => {
                        this.auto_refresh_failed = false;
                        this.replace_chunk(preview, true, window, cx);
                    }
                    Err(error) => this.report_auto_refresh_error(error, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn report_auto_refresh_error(
        &mut self,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.auto_refresh_failed {
            error!(
                operation = "ssh_file_auto_refresh",
                profile_id = %self.profile.id,
                path = %self.entry.path,
                error = %error,
                "refresh remote file tail failed"
            );
            ramag_ui::push_responsive_notification(
                window,
                Notification::error(format!("刷新失败：{error}")),
                cx,
            );
        }
        self.auto_refresh_failed = true;
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
        self.disable_auto_refresh();
        self.chunk_loading = true;
        self.chunk_generation = self.chunk_generation.wrapping_add(1);
        let generation = self.chunk_generation;
        let service = self.service.clone();
        let profile = self.profile.clone();
        let path = self.entry.path.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, async_cx| {
            let result = service.read_file_chunk(&profile, &path, position).await;
            if let Err(error) = &result {
                error!(
                    operation = "ssh_file_chunk_load",
                    profile_id = %profile.id,
                    path = %path,
                    position = ?position,
                    error = %error,
                    "read remote file chunk failed"
                );
            }
            let _ = this.update_in(async_cx, |this, window, cx| {
                if this.chunk_generation != generation {
                    return;
                }
                this.chunk_loading = false;
                match result {
                    Ok(chunk) => match decode_remote_file_chunk(chunk, position) {
                        Ok(preview) => this.replace_chunk(preview, false, window, cx),
                        Err(error) => {
                            error!(
                                operation = "ssh_file_chunk_decode",
                                profile_id = %profile.id,
                                path = %path,
                                position = ?position,
                                error = %error,
                                "decode remote file chunk failed"
                            );
                            ramag_ui::push_responsive_notification(
                                window,
                                Notification::error(error),
                                cx,
                            );
                        }
                    },
                    Err(error) => ramag_ui::push_responsive_notification(
                        window,
                        Notification::error(format!("读取失败：{error}")),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn replace_chunk(
        &mut self,
        preview: RemoteFileText,
        scroll_to_end: bool,
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
        self.auto_refresh_available =
            self.auto_refresh || follow::supports_auto_refresh(&self.entry.path, windowed);
        self.dirty = false;
        self.input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            if scroll_to_end {
                input.set_cursor_position(Position::new(u32::MAX, u32::MAX), window, cx);
            }
        });
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.is_read_only() || !self.is_dirty() {
            return;
        }
        let contents = self.input.read(cx).value().to_string();
        if contents.len() > MAX_REMOTE_FILE_PREVIEW_BYTES {
            ramag_ui::push_responsive_notification(
                window,
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
            if let Err(error) = &result {
                error!(
                    operation = "ssh_file_save",
                    profile_id = %profile.id,
                    path = %path,
                    expected_bytes = expected.len(),
                    written_bytes = bytes.len(),
                    error = %error,
                    "save remote file failed"
                );
            }
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
                        ramag_ui::push_responsive_notification(
                            window,
                            Notification::success("已保存").autohide(true),
                            cx,
                        );
                    }
                    Err(error) => ramag_ui::push_responsive_notification(
                        window,
                        Notification::error(format!("保存失败：{error}")),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn request_close(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            ramag_ui::push_responsive_notification(window, Notification::warning("正在保存"), cx);
        } else if self.is_dirty() {
            ramag_ui::push_responsive_notification(window, Notification::warning("修改未保存"), cx);
        } else {
            window.close_dialog(cx);
        }
    }
}
