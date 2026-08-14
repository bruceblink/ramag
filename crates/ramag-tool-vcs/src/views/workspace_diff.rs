use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _,
    clipboard::Clipboard, h_flex, v_flex,
};

use super::helpers::{FileTabSource, GroupKind};
use super::vcs_view::VcsView;

impl VcsView {
    pub(super) fn render_diff_block(&self, cx: &mut Context<Self>) -> AnyElement {
        // 提前 clone 主题字段，避免后续 cx.listener 借用冲突
        let (fg, muted_fg, accent, muted_bg, border, mono) = {
            let theme = cx.theme();
            (
                theme.foreground,
                theme.muted_foreground,
                theme.accent,
                theme.muted,
                theme.border,
                theme.mono_font_family.clone(),
            )
        };

        let active_tab = self.active_file_tab_idx.and_then(|i| self.file_tabs.get(i));
        let Some(tab) = active_tab else {
            self.diff_layout_cache.borrow_mut().take();
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .px(px(40.0))
                .text_sm()
                .text_color(muted_fg)
                .child("选中左侧文件查看变更")
                .into_any_element();
        };
        let blame_supported = match &tab.source {
            FileTabSource::Changes(GroupKind::Unstaged) => true,
            // HEAD blame 读取工作区；若同文件还有 unstaged 内容，它与 index 新侧行号已不一致。
            FileTabSource::Changes(GroupKind::Staged) => {
                self.status.as_ref().is_some_and(|status| {
                    status
                        .files
                        .iter()
                        .find(|file| file.path == tab.path)
                        .is_some_and(|file| file.unstaged.is_none())
                })
            }
            _ => false,
        };
        let enable_hunk_ops = matches!(
            &tab.source,
            FileTabSource::Changes(GroupKind::Staged | GroupKind::Unstaged)
        );
        let (path, kind, kind_tag): (String, GroupKind, String) = match &tab.source {
            FileTabSource::Changes(k) => {
                let tag = match k {
                    GroupKind::Staged => "已暂存",
                    GroupKind::Unstaged => "未暂存",
                    GroupKind::Untracked => "未跟踪",
                    GroupKind::Conflict => "冲突",
                };
                (tab.path.clone(), *k, tag.to_string())
            }
            FileTabSource::Commit { commit_id, .. } => {
                let short: String = commit_id.chars().take(7).collect();
                (
                    tab.path.clone(),
                    GroupKind::Staged, // 占位：Commit diff 走只读路径，kind 仅用作 enum 必填字段
                    format!("Commit {short}"),
                )
            }
            FileTabSource::ProjectFiles => {
                self.diff_layout_cache.borrow_mut().take();
                return div().into_any_element();
            }
        };
        let kind_copy = kind;
        let header = self.render_diff_header(
            &kind_tag,
            &path,
            kind_copy,
            blame_supported,
            fg,
            accent,
            mono.clone(),
            border,
            cx,
        );
        let body = self.render_diff_body(
            kind_copy,
            blame_supported,
            enable_hunk_ops,
            mono.clone(),
            fg,
            muted_fg,
            muted_bg,
            accent,
            cx,
        );
        let body_layout: AnyElement = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .child(body)
            .into_any_element();

        let mut col = v_flex().size_full().min_w_0().child(header);
        if let Some(blame_text) = &self.inline_blame_text {
            col = col.child(render_inline_blame_banner(
                blame_text.clone(),
                accent,
                fg,
                mono,
                cx,
            ));
        }
        col.child(body_layout).into_any_element()
    }

    pub(super) fn render_file_tab_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.file_tabs.is_empty() {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let accent = theme.accent;
        let muted_bg = theme.muted;
        let mut accent_bg = accent;
        accent_bg.a = 0.12;

        let mut bar = h_flex()
            .id("vcs-ftab-bar")
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .overflow_x_scroll()
            .track_scroll(&self.file_tabs_h_scroll);

        for (idx, tab) in self.file_tabs.iter().enumerate() {
            let is_active = self.active_file_tab_idx == Some(idx);
            let filename = SharedString::from(super::inline_text_preview(
                tab.path.split('/').next_back().unwrap_or(&tab.path),
                160,
            ));
            let tab_id = SharedString::from(format!("vcs-ftab-{idx}"));
            let close_id = SharedString::from(format!("vcs-ftab-close-{idx}"));
            let dot_color = match &tab.source {
                FileTabSource::Changes(GroupKind::Staged) => accent,
                FileTabSource::Changes(GroupKind::Unstaged) => {
                    gpui::hsla(40.0 / 360.0, 0.7, 0.55, 1.0)
                }
                FileTabSource::Changes(GroupKind::Untracked) => muted_fg,
                FileTabSource::Changes(GroupKind::Conflict) => gpui::hsla(0.0, 0.65, 0.55, 1.0),
                FileTabSource::ProjectFiles => gpui::hsla(210.0 / 360.0, 0.6, 0.55, 1.0),
                FileTabSource::Commit { .. } => gpui::hsla(280.0 / 360.0, 0.55, 0.55, 1.0),
            };
            // Changes / Commit 的圆点表达来源状态；Project Files 只在尚未落盘时显示。
            let show_dot = !matches!(tab.source, FileTabSource::ProjectFiles) || tab.is_dirty();
            let path_for_click = tab.path.clone();
            let source_for_click = tab.source.clone();

            let mut tab_el = h_flex()
                .id(tab_id)
                .items_center()
                .gap(px(4.0))
                .px(px(10.0))
                .py(px(4.0))
                .border_r_1()
                .border_color(border)
                .cursor_pointer()
                .when(show_dot, |tab| {
                    tab.child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(dot_color))
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(if is_active { fg } else { muted_fg })
                        .child(filename),
                )
                .child(
                    ramag_ui::clickable_button(close_id)
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("关闭差异")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            // 阻止冒泡到 tab 的 select on_click（否则关了又被重新打开 = 关不掉）
                            cx.stop_propagation();
                            this.close_file_tab(idx, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    match source_for_click.clone() {
                        FileTabSource::Changes(kind) => {
                            this.select_file(path_for_click.clone(), kind, cx);
                        }
                        FileTabSource::ProjectFiles => {
                            this.select_pf_file(path_for_click.clone(), cx);
                        }
                        FileTabSource::Commit { commit_id, .. } => {
                            this.select_commit_file(path_for_click.clone(), commit_id, cx);
                        }
                    }
                }));

            tab_el = if is_active {
                tab_el.bg(accent_bg)
            } else {
                tab_el.hover(move |s| s.bg(muted_bg))
            };
            bar = bar.child(tab_el);
        }
        bar.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_diff_header(
        &self,
        kind_tag: &str,
        path: &str,
        _kind: GroupKind,
        blame_supported: bool,
        fg: gpui::Hsla,
        accent: gpui::Hsla,
        mono: SharedString,
        border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 当前 driver 只支持 HEAD blame；commit 历史 diff 禁用，避免把当前作者冒充历史作者。
        let blame_btn = ramag_ui::clickable_button("vcs-diff-blame-toggle")
            .ghost()
            .xsmall()
            .icon(IconName::Eye)
            .tooltip(if !blame_supported {
                "当前视图不支持"
            } else if self.loading_blame {
                "加载中"
            } else {
                "Blame"
            })
            .disabled(!blame_supported || self.loading_blame)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_blame(cx);
            }));
        let is_full = matches!(self.diff_view_mode, super::helpers::DiffViewMode::FullFile);
        let view_mode_btn = ramag_ui::clickable_button("vcs-diff-view-mode")
            .ghost()
            .xsmall()
            .icon(if is_full {
                ramag_ui::icons::list_filter()
            } else {
                ramag_ui::icons::scroll_text()
            })
            .tooltip(if is_full { "标准" } else { "全文" })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.set_diff_view_mode(this.diff_view_mode.toggled(), cx);
            }));
        let fullscreen_btn = ramag_ui::clickable_button("vcs-diff-fullscreen")
            .ghost()
            .xsmall()
            .icon(if self.diff_fullscreen {
                IconName::Minimize
            } else {
                IconName::Maximize
            })
            .tooltip(if self.diff_fullscreen {
                "退出全屏"
            } else {
                "全屏查看"
            })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_diff_fullscreen(cx);
            }));
        let copy_button = self
            .current_diff
            .as_ref()
            .filter(|diff| {
                !diff.binary
                    && !diff.hunks.is_empty()
                    && super::vcs_view_ops_patch::can_build_patch_for_diff(diff)
            })
            .map(|diff| {
                let diff = diff.clone();
                Clipboard::new("vcs-diff-copy")
                    .tooltip("复制完整 Diff")
                    .value_fn(move |_, _| {
                        super::vcs_view_ops_patch::build_patch_for_diff(&diff)
                            .unwrap_or_default()
                            .into()
                    })
            });
        let mut header = h_flex()
            .gap(px(6.0))
            .items_center()
            .px(px(10.0))
            .py(px(5.0))
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .px(px(6.0))
                    .py(px(1.0))
                    .rounded(px(3.0))
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(accent)
                    .bg({
                        let mut c = accent;
                        c.a = 0.14;
                        c
                    })
                    .child(kind_tag.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(fg)
                    .font_family(mono)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(path.to_string()),
            )
            .when(self.loading_blame, |row| {
                row.child(div().text_xs().text_color(accent).child("blame 加载中…"))
            })
            .child(blame_btn)
            .child(view_mode_btn);
        if let Some(copy_button) = copy_button {
            header = header.child(copy_button);
        }
        header.child(fullscreen_btn).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_diff_body(
        &self,
        kind: GroupKind,
        blame_supported: bool,
        enable_hunk_ops: bool,
        mono: SharedString,
        fg: gpui::Hsla,
        muted_fg: gpui::Hsla,
        muted_bg: gpui::Hsla,
        _accent: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.loading_diff {
            self.diff_layout_cache.borrow_mut().take();
            return placeholder("拉取中…", muted_fg);
        }
        if matches!(kind, GroupKind::Conflict) {
            self.diff_layout_cache.borrow_mut().take();
            return placeholder("（点击左侧冲突文件行，直接打开三栏冲突解决器）", muted_fg);
        }
        let Some(d) = &self.current_diff else {
            self.diff_layout_cache.borrow_mut().take();
            return placeholder("（无差异）", muted_fg);
        };
        let enable_discard = enable_hunk_ops;
        // render 期间 entity 已被 mut 借用，状态必须从 &self 读出后传给纯函数渲染器
        let has_blame = self.showing_blame && !self.blame_lines.is_empty();
        let collapse = matches!(self.diff_view_mode, super::helpers::DiffViewMode::Standard);
        let layout = super::diff_panel_split::prepare_diff_layout(
            &self.diff_layout_cache,
            d,
            false,
            collapse,
            &self.expanded_diff_spacers,
        );
        super::diff_panel_split::render_file_diff_split(
            d,
            self.current_diff_syntax.clone(),
            layout,
            enable_discard,
            mono,
            fg,
            muted_fg,
            muted_bg,
            &self.diff_scroll, // 两栏共享垂直 handle 保证行级同步
            &self.diff_h_scroll,
            has_blame,
            blame_supported,
            cx,
        )
    }
}

fn placeholder(text: &'static str, muted_fg: gpui::Hsla) -> AnyElement {
    div()
        .px(px(12.0))
        .py(px(20.0))
        .text_sm()
        .text_color(muted_fg)
        .child(text)
        .into_any_element()
}

fn render_inline_blame_banner(
    text: SharedString,
    accent: gpui::Hsla,
    fg: gpui::Hsla,
    mono: SharedString,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let mut chip_bg = accent;
    chip_bg.a = 0.10;
    h_flex()
        .w_full()
        .flex_none()
        .px(px(12.0))
        .py(px(4.0))
        .gap(px(8.0))
        .items_center()
        .bg(chip_bg)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_family(mono)
                .text_color(fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(text),
        )
        .child(
            ramag_ui::clickable_button("vcs-inline-blame-close")
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip("关闭行追溯")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.clear_inline_blame(cx);
                })),
        )
        .into_any_element()
}
