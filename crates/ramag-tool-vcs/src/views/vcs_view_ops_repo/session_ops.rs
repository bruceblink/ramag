//! 仓库会话与草稿持久化。

use super::*;

impl VcsView {
    pub(in crate::views) fn remove_open_repo(&mut self, path: String, cx: &mut Context<Self>) {
        self.startup_repo_restore_allowed = false;
        if self.busy || self.loading {
            self.notify_warning("当前操作尚未完成，完成后再关闭仓库标签", cx);
            return;
        }
        let Some(repo_id) = self
            .open_repos
            .iter()
            .find(|repo| repo.path == path)
            .map(|repo| repo.id.clone())
        else {
            self.notify_warning("仓库标签已不存在", cx);
            return;
        };
        let is_current = self.repo.as_ref().map(|r| r.path == path).unwrap_or(false);
        if is_current {
            if !self.ensure_commit_draft_within_limit(cx)
                || !self.ensure_project_file_drafts_saved(cx)
            {
                return;
            }
            // 关闭前立即落盘草稿；关闭失败时仍可继续当前会话。
            self.save_current_session_to_cache(cx);
        }
        self.loading = true;
        self.loading_label = Some("正在关闭仓库…".into());
        cx.notify();

        let driver = self.driver.clone();
        cx.spawn(async move |this, cx| {
            let result = driver.close_repo(&repo_id).await;
            if let Err(error) = &result {
                error!(
                    operation = "git_repo_close",
                    repo_id = %repo_id,
                    error = %error,
                    "close repository failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.loading_label = None;
                match result {
                    Ok(()) => {
                        this.open_repos.retain(|repo| repo.path != path);
                        // 与数据库连接标签一致：关闭即销毁该标签的布局会话，
                        // 再次打开从默认分栏宽度开始。
                        this.repo_session_cache.remove(&path);
                        this.repo_session_order.retain(|entry| entry != &path);
                        this.persist_open_repos(cx);
                        if is_current {
                            let next = this.open_repos.first().cloned();
                            // 先清掉当前仓库，避免打开相邻标签时再次缓存刚关闭的会话。
                            this.reset_session_state(cx);
                            if let Some(next) = next {
                                this.open_recent_repo(next.path, cx);
                            }
                        } else {
                            cx.notify();
                        }
                    }
                    Err(e) => {
                        this.error = Some(format!("关闭仓库失败：{e}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(in crate::views) fn reset_session_state(&mut self, cx: &mut Context<Self>) {
        self.fs_watcher = None;
        self.clear_session_data();
        self.repo = None;
        self.status = None;
        self.local_branches.clear();
        self.remote_branches.clear();
        self.active_view = ActiveView::RepoList;
        cx.notify();
    }

    pub(in crate::views) fn save_current_session_to_cache(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.repo.as_ref().map(|r| r.path.clone()) else {
            return;
        };
        let commit_text = self.commit_input.read(cx).value();
        debug_assert!(commit_text.len() <= MAX_COMMIT_MESSAGE_BYTES);
        // 切仓时立即持久化草稿。
        let generation = self
            .commit_draft_gen
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let generation_ref = self.commit_draft_gen.clone();
        {
            let storage = self.storage.clone();
            let write_lock = self.commit_draft_write_lock.clone();
            let key = commit_draft_pref_key(&path);
            let text = commit_text.clone();
            cx.spawn(async move |this, cx| {
                let _guard = write_lock.lock().await;
                // 不同仓库使用独立键。
                let result = storage.set_preference(&key, &text).await;
                if let Err(error) = &result {
                    tracing::warn!(
                        operation = "git_commit_draft_save",
                        reason = "repo_switch",
                        error = %error,
                        "persist commit draft on switch failed"
                    );
                }
                let _ = this.update(cx, |this, cx| {
                    if generation_ref.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if this.commit_draft_error.take().is_some() {
                                cx.notify();
                            }
                        }
                        Err(error) => {
                            this.commit_draft_error = Some(format!("提交草稿保存失败：{error}"));
                            cx.notify();
                        }
                    }
                });
            })
            .detach();
        }
        // 缓存仅保留标签元数据。
        let mut file_tabs = self.file_tabs.clone();
        strip_file_tab_payloads(&mut file_tabs);
        cache_repo_session(
            &mut self.repo_session_cache,
            &mut self.repo_session_order,
            path,
            RepoSessionState {
                file_tabs,
                active_file_tab_idx: self.active_file_tab_idx,
                commit_text,
                commit_amend: self.commit_amend,
                commit_sign: self.commit_sign,
                ide_left_resize: Some(self.ide_left_resize.clone()),
                ide_files_resize: Some(self.ide_files_resize.clone()),
                detail_resize: Some(self.detail_resize.clone()),
            },
        );
    }

    /// 延迟保存提交草稿。
    pub(in crate::views) fn schedule_commit_draft_persist(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(path) = self.repo.as_ref().map(|repo| repo.path.clone()) else {
            return;
        };
        let text = self.commit_input.read(cx).value();
        let generation = self
            .commit_draft_gen
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let generation_ref = self.commit_draft_gen.clone();
        if text.len() > MAX_COMMIT_MESSAGE_BYTES {
            self.commit_draft_error = Some(format!(
                "提交草稿超过 {} MiB 上限，未保存；请缩短后重试",
                MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
            ));
            cx.notify();
            return;
        }
        let storage = self.storage.clone();
        let write_lock = self.commit_draft_write_lock.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(800))
                .await;
            if generation_ref.load(Ordering::Relaxed) != generation {
                return;
            }
            let _guard = write_lock.lock().await;
            if generation_ref.load(Ordering::Relaxed) != generation {
                return;
            }
            let result = storage
                .set_preference(&commit_draft_pref_key(&path), &text)
                .await;
            if let Err(error) = &result {
                tracing::warn!(
                    operation = "git_commit_draft_save",
                    path = %path,
                    error = %error,
                    "persist commit draft failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                if generation_ref.load(Ordering::Relaxed) != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        if this.commit_draft_error.take().is_some() {
                            cx.notify();
                        }
                    }
                    Err(error) => {
                        this.commit_draft_error = Some(format!("提交草稿保存失败：{error}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(in crate::views) fn restore_session_from_cache(
        &mut self,
        path: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let cached = self.repo_session_cache.get(path).cloned();
        if cached.is_some() {
            touch_repo_session(&mut self.repo_session_order, path);
        }
        match cached {
            Some(mut state) => {
                // 恢复标签但丢弃内容缓存。
                for tab in &mut state.file_tabs {
                    tab.cached_diff = None;
                    tab.cached_diff_syntax = None;
                    tab.cached_content = None;
                }
                self.file_tabs = state.file_tabs;
                self.active_file_tab_idx = state.active_file_tab_idx;
                self.commit_amend = state.commit_amend;
                self.commit_sign = state.commit_sign;
                self.ide_left_resize = state
                    .ide_left_resize
                    .unwrap_or_else(|| cx.new(|_| ResizableState::default()));
                self.ide_files_resize = state
                    .ide_files_resize
                    .unwrap_or_else(|| cx.new(|_| ResizableState::default()));
                self.detail_resize = state
                    .detail_resize
                    .unwrap_or_else(|| cx.new(|_| ResizableState::default()));
                // 清除前一仓库的输入残留。
                self.pending_commit_text = Some(state.commit_text);
                if let Some(idx) = self.active_file_tab_idx
                    && let Some(tab) = self.file_tabs.get(idx).cloned()
                {
                    self.activate_file_tab_state(tab);
                }
                true
            }
            None => {
                self.commit_amend = false;
                self.commit_sign = false;
                self.ide_left_resize = cx.new(|_| ResizableState::default());
                self.ide_files_resize = cx.new(|_| ResizableState::default());
                self.detail_resize = cx.new(|_| ResizableState::default());
                self.pending_commit_text = Some(gpui::SharedString::default());
                false
            }
        }
    }
}
