use super::*;

impl VcsView {
    pub(super) fn render_change_row(
        &self,
        i: usize,
        row: &ChangeRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            ChangeRow::Header {
                title,
                kind,
                file_indices,
            } => self.render_change_header_row(title, *kind, file_indices, cx),
            ChangeRow::Dir {
                display_name,
                dir_path,
                depth,
                is_collapsed,
                file_count,
            } => self.render_change_dir_row(
                i,
                display_name,
                dir_path,
                *depth,
                *is_collapsed,
                *file_count,
                cx,
            ),
            ChangeRow::File {
                file_index,
                depth,
                kind,
            } => self
                .status
                .as_ref()
                .and_then(|status| status.files.get(*file_index))
                .map_or_else(
                    || div().h(px(ROW_H)).into_any_element(),
                    |file| {
                        div()
                            .w_full()
                            .h(px(ROW_H))
                            .flex_none()
                            .pl(px((*depth as f32) * 12.0))
                            .child(self.render_file_row(i, file, *kind, cx))
                            .into_any_element()
                    },
                ),
        }
    }

    pub(super) fn render_change_header_row(
        &self,
        title: &'static str,
        kind: GroupKind,
        file_indices: &Rc<Vec<usize>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let busy = self.busy;
        let count = file_indices.len();
        let badge_color = match kind {
            GroupKind::Conflict => theme.danger,
            GroupKind::Staged => theme.accent,
            GroupKind::Unstaged => gpui::hsla(40.0 / 360.0, 0.7, 0.55, 1.0),
            GroupKind::Untracked => muted_fg,
        };
        let mut badge_bg = badge_color;
        badge_bg.a = 0.14;

        let bulk_btn: Option<AnyElement> = match kind {
            GroupKind::Unstaged | GroupKind::Untracked if !file_indices.is_empty() => {
                Some(bulk_op_button(
                    "stage-all",
                    title,
                    "全暂存",
                    FileOp::Stage,
                    IconName::Plus,
                    file_indices.clone(),
                    busy,
                    cx,
                ))
            }
            GroupKind::Staged if !file_indices.is_empty() => Some(bulk_op_button(
                "unstage-all",
                title,
                "全取消",
                FileOp::Unstage,
                IconName::Minus,
                file_indices.clone(),
                busy,
                cx,
            )),
            _ => None,
        };

        let mut row = h_flex()
            .h(px(ROW_H))
            .flex_none()
            .w_full()
            .gap(px(8.0))
            .items_center()
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .px(px(8.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(badge_color)
                    .bg(badge_bg)
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("{count} 个文件")),
            );
        if let Some(btn) = bulk_btn {
            row = row.child(btn);
        }
        row.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_change_dir_row(
        &self,
        i: usize,
        display_name: &str,
        dir_path: &str,
        depth: usize,
        is_collapsed: bool,
        file_count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let hover_bg = theme.muted;
        let id = SharedString::from(format!("vcs-ch-dir-{i}"));
        let icon = if is_collapsed { "▸" } else { "▾" };
        let dir_clone = dir_path.to_string();
        h_flex()
            .id(id)
            .h(px(ROW_H))
            .flex_none()
            .w_full()
            .gap(px(4.0))
            .items_center()
            .pr(px(6.0))
            .pl(px((4 + depth * 12) as f32))
            .rounded(px(3.0))
            .cursor_pointer()
            .hover(move |this| this.bg(hover_bg))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_changes_dir(dir_clone.clone(), cx);
            }))
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .child(icon),
            )
            .child(
                Icon::new(if is_collapsed {
                    IconName::FolderClosed
                } else {
                    IconName::FolderOpen
                })
                .xsmall()
                .text_color(fg),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(fg)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(super::super::inline_text_preview(display_name, 160)),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("{file_count}")),
            )
            .into_any_element()
    }
}
