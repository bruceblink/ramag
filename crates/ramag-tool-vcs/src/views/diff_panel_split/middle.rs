use super::*;

/// 构建中间列 uniform_list（与左右栏共享 scroll_v 做垂直同步）
///
/// Header 行承载「回滚此 hunk」按钮（enable_discard 时）；Pair 行展示该行 blame author（has_blame 时）
#[allow(clippy::too_many_arguments)]
pub(super) fn build_middle_list(
    total: usize,
    diff_rc: Rc<FileDiff>,
    keys: Rc<Vec<SplitKey>>,
    button_rows: Rc<HashMap<usize, usize>>,
    enable_discard: bool,
    has_blame: bool,
    middle_w: f32,
    scroll_v: UniformListScrollHandle,
    cx: &mut Context<VcsView>,
) -> gpui::UniformList {
    uniform_list(
        "vcs-diff-middle",
        total,
        cx.processor(move |this, range: Range<usize>, _w, cx| {
            let theme = cx.theme();
            let muted_fg = theme.muted_foreground;
            let muted_bg = theme.muted;
            // blame 仅在开启 blame 且已加载时取数据，按 new_lineno 匹配
            let blame_rc: Option<Rc<Vec<ramag_domain::entities::BlameLine>>> =
                if has_blame && this.showing_blame && !this.blame_lines.is_empty() {
                    Some(this.blame_lines.clone())
                } else {
                    None
                };
            let staged_diff = this.active_changes_kind_is_staged();
            let busy = this.busy;
            range
                .map(|i| {
                    if i == keys.len() {
                        return div().w_full().h(px(DIFF_ROW_H)).into_any_element();
                    }
                    // hunk 中点行 + 可回滚：渲染居中回滚按钮（替换该行 blame，仿 VSCode）
                    if enable_discard && let Some(&hunk_idx) = button_rows.get(&i) {
                        return render_middle_revert(hunk_idx, staged_diff, busy, cx);
                    }
                    match keys[i] {
                        SplitKey::Header { .. } => div()
                            .w_full()
                            .h(px(DIFF_ROW_H))
                            .bg(muted_bg)
                            .into_any_element(),
                        SplitKey::Pair {
                            hunk_idx,
                            left,
                            right,
                        } => {
                            // 左列=旧侧行作者、右列=新侧行作者（都按 new_lineno 查当前文件 blame）
                            let author_of = |li: Option<usize>| {
                                li.and_then(|i| diff_rc.hunks[hunk_idx].lines[i].new_lineno)
                                    .and_then(|ln| {
                                        blame_rc.as_ref().and_then(|bs| {
                                            bs.binary_search_by_key(&ln, |blame| blame.line_no)
                                                .ok()
                                                .and_then(|index| bs.get(index))
                                                .map(|b| {
                                                    super::super::inline_text_preview(&b.author, 10)
                                                })
                                        })
                                    })
                            };
                            render_middle_cell(author_of(left), author_of(right), muted_fg)
                        }
                        SplitKey::Spacer { .. } => div()
                            .h(px(DIFF_ROW_H))
                            .w_full()
                            .bg(muted_bg)
                            .into_any_element(),
                    }
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(&scroll_v)
    .w(px(middle_w))
    .h_full()
    .min_h_0()
}

/// 中间列 hunk 操作按钮：放在 hunk 中点行、水平居中（仿 VSCode；仅 enable_discard 时渲染到此）。
/// 未暂存：暂存此 hunk（部分暂存核心操作）+ 丢弃（不可恢复，经确认）；已暂存：移出暂存区
pub(super) fn render_middle_revert(
    hunk_idx: usize,
    staged: bool,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let mut row = h_flex()
        .w_full()
        .h(px(DIFF_ROW_H))
        .items_center()
        .justify_center()
        .gap(px(2.0));
    if !staged {
        row = row.child(
            ramag_ui::clickable_button(SharedString::from(format!("vcs-hunk-stage-{hunk_idx}")))
                .ghost()
                .xsmall()
                .icon(gpui_component::IconName::Plus)
                .tooltip("暂存")
                .disabled(busy)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.stage_hunk(hunk_idx, cx);
                })),
        );
    }
    let tip = if staged { "取消暂存" } else { "丢弃" };
    row.child(
        ramag_ui::clickable_button(SharedString::from(format!("vcs-hunk-discard-{hunk_idx}")))
            .ghost()
            .xsmall()
            .icon(gpui_component::IconName::Undo)
            .tooltip(tip)
            .disabled(busy)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.confirm_discard_hunk(hunk_idx, window, cx);
            })),
    )
    .into_any_element()
}

/// 中间列配对行：左列=旧侧作者、右列=新侧作者（删除行的旧作者需历史 blame，暂空）
pub(super) fn render_middle_cell(
    left_author: Option<String>,
    right_author: Option<String>,
    muted_fg: gpui::Hsla,
) -> AnyElement {
    let col = |author: Option<String>| {
        div()
            .flex_1()
            .min_w_0()
            .px(px(3.0))
            .text_xs()
            .text_color(muted_fg)
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .child(author.unwrap_or_default())
    };
    let mut sep = muted_fg;
    sep.a = 0.25;
    h_flex()
        .w_full()
        .h(px(DIFF_ROW_H))
        .items_center()
        .child(col(left_author))
        .child(div().flex_none().w(px(1.0)).h(px(DIFF_ROW_H)).bg(sep))
        .child(col(right_author))
        .into_any_element()
}
