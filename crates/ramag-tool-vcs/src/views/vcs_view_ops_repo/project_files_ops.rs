//! Project Files 选择、编辑、自动保存与刷新。

use super::*;

impl VcsView {
    pub(in crate::views) fn refresh_current_files_view(&mut self, cx: &mut Context<Self>) {
        match self.files_view_mode {
            FilesViewMode::Changes => self.reload_status_silent(cx),
            FilesViewMode::Stash => self.reload_stashes(cx),
            FilesViewMode::Project => self.reload_project_files(cx),
        }
    }

    pub(in crate::views) fn reload_project_files(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.loading_project_files = true;
        self.project_files_request_seq = self.project_files_request_seq.wrapping_add(1);
        let request_seq = self.project_files_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.list_files(&repo).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.project_files_request_seq != request_seq {
                    return;
                }
                this.loading_project_files = false;
                match result {
                    Ok(paths) => this.project_files = paths,
                    Err(e) => {
                        error!(
                            operation = "vcs_project_files_load",
                            repo_id = %repo,
                            error = %e,
                            "load project files failed"
                        );
                        this.project_files = Vec::new();
                        this.error = Some(format!("加载 Project Files 失败: {e}"));
                    }
                }
                this.prune_project_expanded_dirs();
                this.project_files_version = this.project_files_version.wrapping_add(1);
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::views) fn select_pf_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((repo_path, repo_id)) = self
            .repo
            .as_ref()
            .map(|repo| (repo.path.clone(), repo.id.clone()))
        else {
            return;
        };
        let existing = self
            .file_tabs
            .iter()
            .position(|t| t.path == path && t.source == FileTabSource::ProjectFiles);
        let same_target = existing.is_some_and(|idx| {
            self.active_file_tab_idx == Some(idx)
                && self.selected_pf_path.as_deref() == Some(path.as_str())
        });
        if same_target
            && (existing
                .and_then(|idx| self.file_tabs.get(idx))
                .is_some_and(|tab| tab.cached_content.is_some())
                || self.loading_file_content)
        {
            return;
        }
        self.capture_active_project_draft(cx);
        if self.selected_pf_path.as_deref() != Some(path.as_str()) {
            self.pf_show_source = false;
        }
        self.diff_fullscreen = false;
        if self.viewing_commit.is_some() {
            self.commit_detail_request_seq = self.commit_detail_request_seq.wrapping_add(1);
            self.viewing_commit = None;
            self.reset_commit_files_tree();
            self.selected_commit_file = None;
            self.commit_file_diff = None;
            self.loading_commit_files = false;
        }
        self.file_content_request_seq = self.file_content_request_seq.wrapping_add(1);
        let request_seq = self.file_content_request_seq;
        if self.selected_pf_path.as_deref() != Some(path.as_str()) {
            self.reset_blame_context();
        }
        let is_new_tab = existing.is_none();
        let idx = if let Some(i) = existing {
            i
        } else {
            self.file_tabs.push(FileTab {
                path: path.clone(),
                source: FileTabSource::ProjectFiles,
                cached_diff: None,
                cached_diff_syntax: None,
                cached_content: None,
            });
            self.file_tabs.len() - 1
        };
        if is_new_tab {
            self.scroll_file_tabs_to_end();
        }
        self.active_file_tab_idx = Some(idx);
        let tab = self.file_tabs[idx].clone();
        self.activate_file_tab_state(tab.clone());
        cx.notify();
        if tab.cached_content.is_some() {
            return;
        }

        let repo_root = std::path::PathBuf::from(&repo_path);
        cx.spawn(async move |this, cx| {
            let path_for_worker = path.clone();
            let prepared = match ramag_app::run_blocking(move || {
                let raw = read_raw_file_content(&repo_root, &path_for_worker);
                Ok(prepare_file_snapshot(raw))
            })
            .await
            {
                Ok(prepared) => prepared,
                Err(e) => prepare_file_snapshot(RawFileContent::with_error(
                    path.clone(),
                    format!("文件读取任务失败: {e}"),
                )),
            };
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo_id)
                    || this.file_content_request_seq != request_seq
                    || this.selected_pf_path.as_deref() != Some(path.as_str())
                {
                    return;
                }
                let snapshot = Some(finalize_file_snapshot(prepared));
                if let Some(tab) = this
                    .file_tabs
                    .iter_mut()
                    .find(|t| t.path == path && t.source == FileTabSource::ProjectFiles)
                {
                    tab.cached_content = snapshot.clone();
                }
                this.prune_file_tab_payloads();
                if this.selected_pf_path.as_deref() == Some(path.as_str()) {
                    this.loading_file_content = false;
                    if let Some(snapshot) = snapshot.as_ref() {
                        this.queue_project_editor_load(snapshot);
                    }
                    this.current_file_content = snapshot;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 切换标签前仅更新内存草稿。
    pub(in crate::views) fn capture_active_project_draft(&mut self, cx: &mut Context<Self>) {
        if !self.pf_editor_dirty {
            return;
        }
        let Some(path) = self.pf_editor_loaded_path.clone() else {
            return;
        };
        if self.selected_pf_path.as_deref() != Some(path.as_str()) {
            return;
        }
        let editor = self.pf_editor.read(cx);
        let text = std::rc::Rc::new(editor.value().to_string());
        let line_count = editor.text().len_lines(ropey::LineType::LF);
        let Some(tab) = self
            .file_tabs
            .iter_mut()
            .find(|tab| tab.path == path && tab.source == FileTabSource::ProjectFiles)
        else {
            return;
        };
        let Some(mut snapshot) = tab.cached_content.clone() else {
            return;
        };
        snapshot.text = text;
        snapshot.line_count = line_count;
        snapshot.revision = self.pf_editor_revision;
        snapshot.dirty = true;
        tab.cached_content = Some(snapshot.clone());
        self.pf_editor_line_count = line_count;
        self.current_file_content = Some(snapshot);
    }

    /// 输入时只更新元数据，防抖命中后再复制正文。
    pub(in crate::views) fn mark_active_project_file_dirty(&mut self) {
        let Some(path) = self.pf_editor_loaded_path.as_deref() else {
            return;
        };
        if self.selected_pf_path.as_deref() != Some(path) {
            return;
        }
        let Some(tab) = self
            .file_tabs
            .iter_mut()
            .find(|tab| tab.path == path && tab.source == FileTabSource::ProjectFiles)
        else {
            return;
        };
        let Some(snapshot) = tab.cached_content.as_mut() else {
            return;
        };
        snapshot.line_count = self.pf_editor_line_count;
        snapshot.revision = self.pf_editor_revision;
        snapshot.dirty = true;
        self.current_file_content = Some(snapshot.clone());
    }

    pub(in crate::views) fn queue_project_editor_load(&mut self, snapshot: &FileContentSnapshot) {
        self.pf_editor_loaded_path = None;
        self.pf_editor_dirty = snapshot.dirty;
        self.pf_editor_revision = snapshot.revision;
        self.pf_editor_line_count = snapshot.line_count;
        self.pending_pf_editor_load = Some(PendingFileEditorLoad {
            path: snapshot.path.clone(),
            text: snapshot.text.clone(),
            language: crate::views::syntax::lang_for_path(&snapshot.path)
                .unwrap_or("text")
                .into(),
        });
    }

    pub(in crate::views) fn schedule_project_file_autosave(&mut self, cx: &mut Context<Self>) {
        self.schedule_current_project_file_save(PF_FILE_AUTOSAVE_DEBOUNCE, cx);
    }

    pub(in crate::views) fn save_project_file(&mut self, cx: &mut Context<Self>) {
        self.schedule_current_project_file_save(Duration::ZERO, cx);
    }

    pub(in crate::views) fn schedule_current_project_file_save(
        &mut self,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        if !self.pf_editor_dirty {
            return;
        }
        let Some(path) = self.selected_pf_path.clone() else {
            return;
        };
        let Some(snapshot) = self.current_file_content.as_ref() else {
            return;
        };
        if snapshot.error.is_some() || snapshot.binary || snapshot.truncated || !snapshot.dirty {
            return;
        }
        let Some((repo_path, repo_id)) = self
            .repo
            .as_ref()
            .map(|repo| (repo.path.clone(), repo.id.clone()))
        else {
            return;
        };

        let revision = self.pf_editor_revision;
        let coordinator = self.project_file_write_coordinator.clone();
        let ticket = coordinator.begin(format!("{repo_path}\0{path}"));

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;

            // 仅最新代际复制正文，避免快速输入反复克隆大文本。
            let text = this
                .update(cx, |this, cx| {
                    if !this.is_current_repo(&repo_id) {
                        return None;
                    }
                    if this.selected_pf_path.as_deref() == Some(path.as_str())
                        && this.pf_editor_loaded_path.as_deref() == Some(path.as_str())
                    {
                        if !this.pf_editor_dirty || this.pf_editor_revision != revision {
                            return None;
                        }
                        this.capture_active_project_draft(cx);
                    }
                    let snapshot = this
                        .file_tabs
                        .iter()
                        .find(|tab| tab.path == path && tab.source == FileTabSource::ProjectFiles)?
                        .cached_content
                        .as_ref()?;
                    let text = (snapshot.dirty
                        && snapshot.revision == revision
                        && snapshot.error.is_none()
                        && !snapshot.binary
                        && !snapshot.truncated)
                        .then(|| snapshot.text.as_ref().clone())?;
                    // 写入前登记，确保监听事件只消费一次。
                    let now = std::time::Instant::now();
                    this.project_file_self_writes.retain(|_, (_, saved_at)| {
                        now.saturating_duration_since(*saved_at) <= PF_FILE_SELF_WRITE_TTL
                    });
                    this.project_file_self_writes
                        .insert(path.clone(), (revision, now));
                    Some(text)
                })
                .ok()
                .flatten();
            let Some(text) = text else {
                coordinator.cancel_if_latest(&ticket);
                return;
            };

            let root = std::path::PathBuf::from(repo_path);
            let path_for_worker = path.clone();
            let result = coordinator
                .run_if_latest(&ticket, || {
                    ramag_app::run_blocking(move || {
                        write_project_file(&root, &path_for_worker, text.as_str())
                            .map_err(ramag_domain::error::DomainError::Other)
                    })
                })
                .await;
            let Some(result) = result else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo_id) {
                    return;
                }
                match result {
                    Ok(()) => {
                        // 旧回包不得清除新一代编辑状态。
                        let current =
                            mark_project_file_revision_saved(&mut this.file_tabs, &path, revision);
                        if this.selected_pf_path.as_deref() == Some(path.as_str())
                            && let Some(snapshot) = current
                        {
                            this.pf_editor_dirty = snapshot.dirty;
                            this.current_file_content = Some(snapshot);
                        }
                    }
                    Err(error) => {
                        tracing::error!(
                            operation = "vcs_project_file_autosave",
                            repo_id = %repo_id,
                            path = %path,
                            error = %error,
                            "autosave project file failed"
                        );
                        this.pending_notification = Some(
                            gpui_component::notification::Notification::error(format!(
                                "自动保存 {path} 失败：{error}；可按 {} 重试",
                                ramag_ui::platform::primary_shortcut("S")
                            ))
                            .autohide(true),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
