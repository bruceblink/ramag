//! 分支比较面板：先加载范围文件清单，再按需读取单文件 Diff。

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, div, prelude::*, px,
    uniform_list,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};
use ramag_domain::entities::{FileStatus, contains_case_insensitive};
use tracing::{error, info};

use super::helpers::{FileTabSource, code_letter_color, code_to_letter};
use super::vcs_view::{CompareState, VcsView};

const COMPARE_ROW_H: f32 = 28.0;

impl VcsView {
    /// 打开只读分支比较：目标分支作为起点，当前 HEAD 作为终点。
    pub(in crate::views) fn open_compare(
        &mut self,
        target_label: String,
        target_commit: String,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            self.notify_warning("当前 Git 操作正在执行，请稍后比较", cx);
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|repo| repo.id.clone()) else {
            return;
        };
        let Some(current_commit) = self
            .status
            .as_ref()
            .and_then(|status| status.head_commit.clone())
        else {
            self.notify_warning("当前仓库还没有可比较的 HEAD", cx);
            return;
        };
        if current_commit == target_commit {
            self.notify_warning("当前分支与目标分支指向同一个 commit", cx);
            return;
        }

        // 比较会话切换后，旧范围的标签不能继续代表新分支，先清理活动 diff。
        self.clear_compare_state();
        self.files_view_mode = super::helpers::FilesViewMode::Changes;
        self.compare_request_seq = self.compare_request_seq.wrapping_add(1);
        let request_seq = self.compare_request_seq;
        self.compare = Some(CompareState {
            from: target_commit.clone(),
            to: current_commit.clone(),
            target_label: target_label.clone(),
            files: Rc::new(Vec::new()),
            loading: true,
        });
        self.changes_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        self.error = None;
        cx.notify();

        let driver = self.driver.clone();
        cx.spawn(async move |this, cx| {
            let result = driver
                .list_diff_files(&repo, &target_commit, &current_commit)
                .await;
            let _ = this.update(cx, |this, cx| {
                let is_current = this.is_current_repo(&repo)
                    && this.compare_request_seq == request_seq
                    && this.compare.as_ref().is_some_and(|compare| {
                        compare.from == target_commit && compare.to == current_commit
                    });
                if !is_current {
                    return;
                }
                let Some(compare) = this.compare.as_mut() else {
                    return;
                };
                compare.loading = false;
                match result {
                    Ok(files) => {
                        let count = files.len();
                        compare.files = Rc::new(files);
                        info!(
                            operation = "vcs_compare_files",
                            repo_id = %repo,
                            from = %compare.from,
                            to = %compare.to,
                            file_count = count,
                            status = "completed",
                            "loaded revision range files"
                        );
                    }
                    Err(error) => {
                        error!(
                            operation = "vcs_compare_files",
                            repo_id = %repo,
                            from = %compare.from,
                            to = %compare.to,
                            error = %error,
                            "load revision range files failed"
                        );
                        this.error = Some(format!("加载分支比较失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关闭比较会话并移除属于旧 revision 范围的文件标签。
    pub(in crate::views) fn close_compare(&mut self, cx: &mut Context<Self>) {
        self.clear_compare_state();
        cx.notify();
    }

    /// 清理比较请求和活动状态；切仓或切到其他文件视图时也调用此方法。
    pub(super) fn clear_compare_state(&mut self) {
        self.compare_request_seq = self.compare_request_seq.wrapping_add(1);
        self.compare = None;
        self.file_tabs
            .retain(|tab| !matches!(tab.source, FileTabSource::Compare { .. }));
        self.active_file_tab_idx = None;
        self.selected_file = None;
        self.current_diff = None;
        self.current_diff_syntax = None;
        self.loading_diff = false;
        self.diff_fullscreen = false;
        self.reset_blame_context();
    }

    pub(super) fn render_compare_files_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(compare) = self.compare.as_ref() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let border = theme.border;
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let accent = theme.accent;
        let target_label = super::inline_text_preview(&compare.target_label, 120);
        let from_short = compare.from.chars().take(7).collect::<String>();
        let to_short = compare.to.chars().take(7).collect::<String>();

        let header = h_flex()
            .h(px(36.0))
            .flex_none()
            .w_full()
            .gap(px(6.0))
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(
                Icon::new(ramag_ui::icons::git_compare())
                    .small()
                    .text_color(accent),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(fg)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(format!(
                        "当前分支 ↔ {target_label} · {from_short}..{to_short}"
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("{} 个文件", compare.files.len())),
            )
            .child(
                ramag_ui::clickable_button("vcs-compare-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("关闭比较")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_compare(cx);
                    })),
            );

        if compare.loading {
            return v_flex()
                .size_full()
                .min_h_0()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(muted_fg)
                        .child("加载比较文件…"),
                )
                .into_any_element();
        }

        let query = self
            .files_search_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let files = compare.files.clone();
        let visible_indices = Rc::new(
            files
                .iter()
                .enumerate()
                .filter_map(|(index, file)| {
                    contains_case_insensitive(&file.path, &query).then_some(index)
                })
                .collect::<Vec<_>>(),
        );
        if visible_indices.is_empty() {
            let message = if query.is_empty() {
                "两个 revision 之间没有文件差异"
            } else {
                "没有匹配的比较文件"
            };
            return v_flex()
                .size_full()
                .min_h_0()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(muted_fg)
                        .child(message),
                )
                .into_any_element();
        }

        let body = uniform_list(
            "vcs-compare-files",
            visible_indices.len(),
            cx.processor({
                let files = files.clone();
                let visible_indices = visible_indices.clone();
                let from = compare.from.clone();
                let to = compare.to.clone();
                move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|row_index| {
                            let file_index = *visible_indices.get(row_index)?;
                            let file = files.get(file_index)?;
                            Some(render_compare_file_row(
                                file_index,
                                file,
                                &from,
                                &to,
                                &this.file_tabs,
                                this.active_file_tab_idx,
                                cx,
                            ))
                        })
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.changes_scroll)
        .flex_1();

        v_flex()
            .size_full()
            .min_h_0()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

fn render_compare_file_row(
    index: usize,
    file: &FileStatus,
    from: &str,
    to: &str,
    tabs: &[super::helpers::FileTab],
    active_tab_idx: Option<usize>,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let theme = cx.theme();
    let muted_fg = theme.muted_foreground;
    let fg = theme.foreground;
    let hover_bg = theme.muted;
    let kind = file.staged.or(file.unstaged);
    let code = code_to_letter(kind);
    let code_color = code_letter_color(code, muted_fg);
    let path = file.path.clone();
    let path_label = match (&file.old_path, file.path.as_str()) {
        (Some(old), new) if old != new => format!("{old} → {new}"),
        _ => file.path.clone(),
    };
    let selected = active_tab_idx
        .and_then(|idx| tabs.get(idx))
        .is_some_and(|tab| {
            tab.path == file.path
                && tab.source
                    == (FileTabSource::Compare {
                        from: from.to_string(),
                        to: to.to_string(),
                    })
        });
    let mut selected_bg = theme.accent;
    selected_bg.a = 0.16;
    let row_id = SharedString::from(format!("vcs-compare-file-{index}"));
    let from = from.to_string();
    let to = to.to_string();
    let mut row = h_flex()
        .id(row_id)
        .h(px(COMPARE_ROW_H))
        .flex_none()
        .w_full()
        .gap(px(8.0))
        .items_center()
        .px(px(8.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .hover(move |this| this.bg(hover_bg))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.select_compare_file(path.clone(), from.clone(), to.clone(), cx);
        }))
        .child(Icon::new(IconName::File).xsmall().text_color(muted_fg))
        .child(
            div()
                .w(px(14.0))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(code_color)
                .child(code),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(super::inline_text_preview(&path_label, 240)),
        );
    if selected {
        row = row.bg(selected_bg);
    }
    row.into_any_element()
}
