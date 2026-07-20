//! VcsView 顶层 Render：tab bar + body 路由（RepoList / IDE 布局）

use gpui::{
    AnyElement, Context, Focusable as _, IntoElement, ParentElement, Render, Styled, Window, div,
    prelude::*,
};
use gpui_component::{ActiveTheme, v_flex};
use ramag_domain::entities::MAX_COMMIT_MESSAGE_BYTES;

use super::super::helpers::ActiveView;
use super::VcsView;

impl Render for VcsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 把异步操作完成时挂起的 toast 推送出来（commit / push / pull 等成功提示）
        if let Some(n) = self.pending_notification.take() {
            use gpui_component::WindowExt as _;
            window.push_notification(n, cx);
        }
        // 仓库管理页首次显示即聚焦搜索框，进入页面直接可打字过滤
        if !self.focused_repo_search_once && matches!(self.active_view, ActiveView::RepoList) {
            self.focused_repo_search_once = true;
            self.repo_search_input
                .update(cx, |state, cx| state.focus(window, cx));
        }
        // Clone 取消后的半成品目录：弹确认交用户决定删除或保留（删除是不可逆文件操作，
        // 只对本次 clone 创建的目录发起，绝不触碰既有目录）
        if let Some(dir) = self.pending_clone_cleanup.take() {
            let view = cx.entity();
            cx.defer_in(window, move |_, window, cx| {
                let display = dir.display().to_string();
                ramag_ui::open_confirm(
                    "删除未完成的 Clone 目录？",
                    format!("Clone 已取消，残留半成品目录：\n{display}\n\n删除该目录？选择保留可稍后手动处理。"),
                    "删除",
                    true,
                    move |_, app| {
                        view.update(app, |this, cx| {
                            this.cleanup_cancelled_clone_dir_async(dir, cx);
                        });
                    },
                    window,
                    cx,
                );
            });
        }
        // commit 草稿恢复：仓库切换后用 cx.defer_in 借 Window 写回 InputState
        if let Some(text) = self.pending_commit_text.take() {
            if text.len() > MAX_COMMIT_MESSAGE_BYTES {
                self.pending_notification = Some(
                    gpui_component::notification::Notification::warning(format!(
                        "已忽略超过 {} MiB 上限的提交信息",
                        MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
                    ))
                    .autohide(true),
                );
            } else {
                let input = self.commit_input.clone();
                cx.defer_in(window, move |_, window, cx| {
                    input.update(cx, |state, ctx| {
                        state.set_value(text, window, ctx);
                    });
                });
            }
        }
        // 切仓后清空搜索框：文件搜索与历史搜索是仓库上下文，不跨仓残留
        if self.pending_clear_search_inputs {
            self.pending_clear_search_inputs = false;
            let files_input = self.files_search_input.clone();
            let history_input = self.history_search_input.clone();
            cx.defer_in(window, move |_, window, cx| {
                files_input.update(cx, |state, ctx| {
                    state.set_value("", window, ctx);
                });
                history_input.update(cx, |state, ctx| {
                    state.set_value("", window, ctx);
                });
            });
        }
        if self.pending_clear_creation_inputs {
            self.pending_clear_creation_inputs = false;
            let branch_input = self.create_branch_input.clone();
            let tag_input = self.create_tag_input.clone();
            let tag_message_input = self.create_tag_message_input.clone();
            let remote_name_input = self.create_remote_name_input.clone();
            let remote_url_input = self.create_remote_url_input.clone();
            cx.defer_in(window, move |_, window, cx| {
                for input in [
                    branch_input,
                    tag_input,
                    tag_message_input,
                    remote_name_input,
                    remote_url_input,
                ] {
                    input.update(cx, |state, ctx| {
                        state.set_value("", window, ctx);
                    });
                }
            });
        }
        if let Some(load) = self.pending_pf_editor_load.take() {
            let super::super::helpers::PendingFileEditorLoad {
                path,
                text,
                language,
            } = load;
            let editor = self.pf_editor.clone();
            cx.defer_in(window, move |this, window, cx| {
                if this.selected_pf_path.as_deref() != Some(path.as_str()) {
                    return;
                }
                editor.update(cx, |state, cx| {
                    state.set_highlighter(language, cx);
                    state.set_value(text.as_ref().clone(), window, cx);
                });
                this.pf_editor_loaded_path = Some(path);
                cx.notify();
            });
        }
        let theme = cx.theme();
        let bg = theme.background;
        let muted_fg = theme.muted_foreground;

        // 两层结构（仿 dbclient）：tab bar（含右侧操作区） / body
        // body 由 active_view 路由：RepoList → 仓库管理页；Session → IDE 布局
        // 注意：error 不再独占 body —— 由 RepoList 顶部 banner 承载（不阻塞用户操作）
        let body: AnyElement = if self.loading {
            // Clone 进行中：附加 git --progress 实时行 + 取消按钮（进度槽每帧读取）
            let clone_line = self
                .clone_progress
                .as_ref()
                .and_then(|progress| match progress.try_lock() {
                    Ok(text) => Some(text.clone()),
                    Err(std::sync::TryLockError::WouldBlock) => None,
                    Err(std::sync::TryLockError::Poisoned(error)) => {
                        tracing::warn!("vcs clone progress lock poisoned");
                        Some(error.into_inner().clone())
                    }
                })
                .filter(|s| !s.is_empty());
            let cancel_btn = self.clone_cancel.clone().map(|cancel| {
                ramag_ui::clickable_button("vcs-clone-cancel")
                    .label("取消 Clone")
                    .on_click(move |_: &gpui::ClickEvent, _, _| {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    })
            });
            gpui_component::v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(gpui::px(10.0))
                .text_sm()
                .text_color(muted_fg)
                .child(
                    self.loading_label
                        .clone()
                        .unwrap_or_else(|| "加载中…".to_string()),
                )
                .children(clone_line.map(|l| div().text_xs().text_color(muted_fg).child(l)))
                .children(cancel_btn)
                .into_any_element()
        } else {
            match self.active_view {
                ActiveView::RepoList => self.render_repo_list(cx),
                ActiveView::Session => {
                    if self.repo.is_some() {
                        self.render_ide_layout(cx)
                    } else {
                        // 异常态：active_view=Session 但 repo 不存在 → fallback 列表
                        self.render_repo_list(cx)
                    }
                }
            }
        };

        v_flex()
            .size_full()
            .bg(bg)
            .key_context("VcsView")
            .track_focus(&self.focus_handle)
            // CloseTab：先关文件标签；没有文件标签时关闭当前仓库标签。
            .on_action(cx.listener(|this, _: &ramag_ui::CloseTab, window, cx| {
                if let Some(idx) = this.active_file_tab_idx {
                    this.close_file_tab(idx, cx);
                    window.focus(&this.focus_handle, cx);
                } else if matches!(this.active_view, ActiveView::Session) {
                    if let Some(path) = this.repo.as_ref().map(|repo| repo.path.clone()) {
                        this.remove_open_repo(path, cx);
                    } else {
                        cx.propagate();
                    }
                } else {
                    cx.propagate();
                }
            }))
            // cmd-r：手动刷新工作区
            .on_action(
                cx.listener(|this, _: &crate::actions::RefreshWorkspace, _, cx| {
                    if this.repo.is_some() && !this.loading && !this.busy {
                        this.refresh_workspace_silent(cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::SaveProjectFile, _, cx| {
                    this.save_project_file(cx);
                }),
            )
            // cmd-shift-k / cmd-t：push / pull 当前分支
            .on_action(
                cx.listener(|this, _: &crate::actions::PushNow, window, cx| {
                    if this.repo.is_some() && !this.busy {
                        this.confirm_remote_op(super::super::helpers::RemoteOp::Push, window, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::PullNow, window, cx| {
                    if this.repo.is_some() && !this.busy {
                        this.confirm_remote_op(super::super::helpers::RemoteOp::Pull, window, cx);
                    }
                }),
            )
            // cmd-shift-h：底部历史面板
            .on_action(
                cx.listener(|this, _: &crate::actions::ToggleHistoryPane, _, cx| {
                    if this.repo.is_some() {
                        this.toggle_history_pane(cx);
                    }
                }),
            )
            // cmd-k：切 Changes 并聚焦 commit 输入框
            .on_action(
                cx.listener(|this, _: &crate::actions::FocusCommitMessage, window, cx| {
                    if this.repo.is_none() {
                        return;
                    }
                    this.set_files_view_mode(super::super::helpers::FilesViewMode::Changes, cx);
                    let fh = this.commit_input.read(cx).focus_handle(cx);
                    window.focus(&fh, cx);
                }),
            )
            // cmd-enter：仅 commit 输入框聚焦时提交（其他输入框里不劫持）
            .on_action(
                cx.listener(|this, _: &crate::actions::CommitNow, window, cx| {
                    if this.repo.is_none() || this.busy {
                        return;
                    }
                    let fh = this.commit_input.read(cx).focus_handle(cx);
                    if fh.is_focused(window) {
                        this.confirm_commit(window, cx);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .child(self.render_tabs(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
