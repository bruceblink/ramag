//! 冲突对比：index stage 2（ours）| stage 3（theirs）。整文件采纳 / 手改后标记已解决。

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled,
    UniformListScrollHandle, div, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, v_flex,
};

use super::helpers::ConflictOp;
use super::vcs_view::VcsView;
use super::workspace_conflict::conflict_side_labels;

impl VcsView {
    /// 三方冲突编辑器主入口（IDE 布局 render_main_area 路由调用）
    pub(super) fn render_conflict_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let (border, muted_fg, fg, bg, mono, accent, danger) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.foreground,
                theme.background,
                theme.mono_font_family.clone(),
                theme.accent,
                theme.danger,
            )
        };
        let busy = self.busy;

        if self.loading_conflict {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(div().text_sm().text_color(muted_fg).child("加载冲突内容…"))
                .into_any_element();
        }

        let Some(content) = &self.conflict_content else {
            return div().into_any_element();
        };

        let content = content.clone();
        let path = content.path.clone();
        let ours_len = content.ours.len();
        let theirs_len = content.theirs.len();
        let (ours_label, theirs_label) =
            conflict_side_labels(self.status.as_ref().and_then(|status| status.operation));

        let mut ours_hdr_bg = accent;
        ours_hdr_bg.a = 0.08;
        let mut theirs_hdr_bg = danger;
        theirs_hdr_bg.a = 0.08;

        let header = h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .bg(bg)
            .child(
                ramag_ui::clickable_button("vcs-conflict-close")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .label("关闭")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.conflict_request_seq = this.conflict_request_seq.wrapping_add(1);
                        this.conflict_editor_path = None;
                        this.conflict_content = None;
                        this.loading_conflict = false;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(format!(
                        "冲突对比：{}",
                        super::inline_text_preview(&path, 200)
                    )),
            )
            .child(
                ramag_ui::clickable_button("vcs-conflict-use-ours")
                    .outline()
                    .small()
                    .label("采纳左侧")
                    .disabled(busy)
                    .on_click({
                        let p = path.clone();
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.run_conflict_op(ConflictOp::UseOurs, p.clone(), cx);
                        })
                    }),
            )
            .child(
                ramag_ui::clickable_button("vcs-conflict-use-theirs")
                    .outline()
                    .small()
                    .label("采纳右侧")
                    .disabled(busy)
                    .on_click({
                        let p = path.clone();
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.run_conflict_op(ConflictOp::UseTheirs, p.clone(), cx);
                        })
                    }),
            )
            .child(
                ramag_ui::clickable_button("vcs-conflict-mark-resolved")
                    .primary()
                    .small()
                    .icon(IconName::Check)
                    .label("解决")
                    .disabled(busy)
                    .on_click({
                        let p = path.clone();
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.run_conflict_op(ConflictOp::MarkResolved, p.clone(), cx);
                        })
                    }),
            );

        let body = h_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_r_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .flex_none()
                            .w_full()
                            .px(px(10.0))
                            .py(px(3.0))
                            .bg(ours_hdr_bg)
                            .border_b_1()
                            .border_color(border)
                            .items_center()
                            .gap(px(4.0))
                            .child(Icon::new(IconName::ArrowLeft).xsmall().text_color(accent))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(accent)
                                    .child(format!("{ours_label}（ours）— {ours_len} 行")),
                            ),
                    )
                    .child(div().flex_1().min_h_0().child(lines_panel_virtual(
                        content.clone(),
                        ConflictSide::Ours,
                        mono.clone(),
                        fg,
                        muted_fg,
                        self.conflict_ours_scroll.clone(),
                        cx,
                    ))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .flex_none()
                            .w_full()
                            .px(px(10.0))
                            .py(px(3.0))
                            .bg(theirs_hdr_bg)
                            .border_b_1()
                            .border_color(border)
                            .items_center()
                            .gap(px(4.0))
                            .child(Icon::new(IconName::ArrowRight).xsmall().text_color(danger))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(danger)
                                    .child(format!("{theirs_label}（theirs）— {} 行", theirs_len)),
                            ),
                    )
                    .child(div().flex_1().min_h_0().child(lines_panel_virtual(
                        content,
                        ConflictSide::Theirs,
                        mono,
                        fg,
                        muted_fg,
                        self.conflict_theirs_scroll.clone(),
                        cx,
                    ))),
            );

        v_flex()
            .size_full()
            .min_h_0()
            .bg(bg)
            .child(header)
            .child(body)
            .into_any_element()
    }
}

/// 渲染带行号的纯文本内容列（ours / theirs 两侧共用，uniform_list 行级虚拟化）
const CONFLICT_ROW_H: f32 = 18.0;

fn lines_panel_virtual(
    content: Rc<ramag_domain::entities::ConflictContent>,
    side: ConflictSide,
    mono: SharedString,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    scroll: UniformListScrollHandle,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let total = side.lines(&content).len();
    if total == 0 {
        return div().into_any_element();
    }
    uniform_list(
        side.list_id(),
        total,
        cx.processor({
            let content = content.clone();
            let mono = mono.clone();
            move |_this, range: Range<usize>, _w, _cx| {
                range
                    .map(|i| {
                        let gutter = format!("{:>4}", i + 1);
                        let line = &side.lines(&content)[i];
                        h_flex()
                            .w_full()
                            .h(px(CONFLICT_ROW_H))
                            .flex_none()
                            .items_center()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(36.0))
                                    .px(px(4.0))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(muted_fg)
                                    .child(gutter),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .pr(px(8.0))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(fg)
                                    .child(if line.is_empty() {
                                        "\u{00a0}".into()
                                    } else {
                                        line.clone()
                                    }),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }
        }),
    )
    .track_scroll(&scroll)
    .h_full()
    .flex_1()
    .into_any_element()
}

#[derive(Clone, Copy)]
enum ConflictSide {
    Ours,
    Theirs,
}

impl ConflictSide {
    fn list_id(self) -> &'static str {
        match self {
            Self::Ours => "vcs-conflict-ours",
            Self::Theirs => "vcs-conflict-theirs",
        }
    }

    fn lines(self, content: &ramag_domain::entities::ConflictContent) -> &[String] {
        match self {
            Self::Ours => &content.ours,
            Self::Theirs => &content.theirs,
        }
    }
}
