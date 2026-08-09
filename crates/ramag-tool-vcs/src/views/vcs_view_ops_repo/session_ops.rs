//! 仓库标签页会话、草稿持久化与缓存恢复。

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
            // 关闭前保留会话，进程内重开可恢复。
            self.save_current_session_to_cache(cx);
        }
        self.loading = true;
        self.loading_label = Some("正在关闭仓库…".into());
        cx.notify();

        let driver = self.driver.clone();
        cx.spawn(async move |this, cx| {
            let result = driver.close_repo(&repo_id).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.loading_label = None;
                match result {
                    Ok(()) => {
                        this.open_repos.retain(|repo| repo.path != path);
                        this.persist_open_repos(cx);
                        if is_current {
                            if let Some(next) = this.open_repos.first().cloned() {
                                this.open_recent_repo(next.path, cx);
                            } else {
                                this.reset_session_state(cx);
                            }
                        } else {
                            cx.notify();
                        }
                    }
                    Err(e) => {
                        error!(
                            operation = "git_repo_close",
                            error = %e,
                            repo_id = %repo_id,
                            "close repository failed"
                        );
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
        // 切仓时立即持久化并作废在途防抖任务。
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
                // 不同仓库使用独立键，后续输入不能取消本次落盘。
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
        // 缓存只保留标签元数据，内容切回时重读。
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

    /// 输入停顿后持久化，同代校验防止旧任务覆盖。
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

    /// 返回是否命中缓存。
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
                // 外部可能已修改仓库，恢复标签但丢弃内容缓存。
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
                // 强制覆盖前一个仓库的输入残留。
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
