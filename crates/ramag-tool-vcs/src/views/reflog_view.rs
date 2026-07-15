//! Reflog 视图：每行 hash / selector / action / subject / 时间 + Checkout 按钮（detached HEAD）。
//! uniform_list 行级虚拟化，与 history 互斥

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use ramag_domain::entities::{ReflogEntry, contains_case_insensitive};

use super::vcs_view::VcsView;

/// 每行高度（与 commit 行 28px 对齐，视觉一致）
const ROW_HEIGHT: f32 = 28.0;

pub(super) struct ReflogRowsCacheEntry {
    entries: Rc<Vec<ReflogEntry>>,
    query_lower: String,
    indices: Rc<Vec<usize>>,
}

impl ReflogRowsCacheEntry {
    fn get(&self, entries: &Rc<Vec<ReflogEntry>>, query_lower: &str) -> Option<Rc<Vec<usize>>> {
        (Rc::ptr_eq(&self.entries, entries) && self.query_lower == query_lower)
            .then(|| self.indices.clone())
    }
}

impl VcsView {
    /// reflog 视图主入口
    pub(super) fn render_reflog_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let count = self.reflog_entries.len();

        if self.loading_reflog {
            return center("加载 reflog…", muted_fg);
        }
        if self.reflog_entries.is_empty() {
            return center("(reflog 为空)", muted_fg);
        }

        // 搜索框在 reflog 模式做客户端即时过滤（commit 模式才是 git 侧搜索）
        let query_lower = self
            .history_search_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let entries_rc = self.reflog_entries.clone();
        let indices_rc = self.filtered_reflog_indices(entries_rc.clone(), &query_lower);
        if indices_rc.is_empty() {
            return center("没有匹配的 reflog 记录", muted_fg);
        }
        // 行数按过滤后取；底部统计仍显示总数
        let visible_count = indices_rc.len();
        let busy = self.busy;
        let mono = theme.mono_font_family.clone();
        let fg = theme.foreground;
        let accent = theme.accent;

        let body = uniform_list(
            "vcs-reflog-rows",
            visible_count,
            cx.processor({
                let entries_rc = entries_rc.clone();
                let indices_rc = indices_rc.clone();
                let mono = mono.clone();
                move |_this, range: Range<usize>, _w, cx| {
                    let muted_fg = cx.theme().muted_foreground;
                    let hover_bg = cx.theme().muted;
                    range
                        .map(|i| {
                            let entry_index = indices_rc[i];
                            render_reflog_row(
                                i,
                                &entries_rc[entry_index],
                                busy,
                                fg,
                                muted_fg,
                                accent,
                                hover_bg,
                                mono.clone(),
                                cx,
                            )
                        })
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.reflog_scroll)
        .flex_1();

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted_fg)
                    .pb(px(8.0))
                    .child(format!("HEAD reflog 共 {count} 条")),
            )
            .child(body)
            .into_any_element()
    }

    fn filtered_reflog_indices(
        &self,
        entries: Rc<Vec<ReflogEntry>>,
        query_lower: &str,
    ) -> Rc<Vec<usize>> {
        {
            let cache = self.reflog_rows_cache.borrow();
            if let Some(indices) = cache
                .as_ref()
                .and_then(|entry| entry.get(&entries, query_lower))
            {
                return indices;
            }
        }

        let indices: Rc<Vec<usize>> = Rc::new(
            entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    matches_reflog_query(entry, query_lower).then_some(index)
                })
                .collect(),
        );
        self.reflog_rows_cache.replace(Some(ReflogRowsCacheEntry {
            entries,
            query_lower: query_lower.to_string(),
            indices: indices.clone(),
        }));
        indices
    }
}

fn matches_reflog_query(entry: &ReflogEntry, query_lower: &str) -> bool {
    query_lower.is_empty()
        || contains_case_insensitive(&entry.subject, query_lower)
        || contains_case_insensitive(&entry.action, query_lower)
        || contains_case_insensitive(&entry.selector, query_lower)
        || entry.commit.0.starts_with(query_lower)
}

/// 单条 reflog 行渲染（在 uniform_list closure 内调）
#[allow(clippy::too_many_arguments)]
fn render_reflog_row(
    idx: usize,
    e: &ReflogEntry,
    busy: bool,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    hover_bg: gpui::Hsla,
    mono: SharedString,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let short_hash = if e.commit.0.len() > 7 {
        &e.commit.0[..7]
    } else {
        e.commit.0.as_str()
    };
    let time_str = e.timestamp.format("%m-%d %H:%M").to_string();
    let action_color = match e.action.as_str() {
        "commit" | "commit (initial)" | "commit (amend)" => accent,
        "checkout" => gpui::hsla(220.0 / 360.0, 0.6, 0.55, 1.0),
        "reset" => gpui::hsla(0.0, 0.65, 0.55, 1.0),
        "merge" | "rebase" | "rebase (start)" | "rebase (finish)" => {
            gpui::hsla(280.0 / 360.0, 0.55, 0.55, 1.0)
        }
        _ => muted_fg,
    };
    let commit_for_btn = e.commit.0.clone();
    let row_id = SharedString::from(format!("vcs-reflog-row-{idx}"));

    h_flex()
        .id(row_id)
        .h(px(ROW_HEIGHT))
        .flex_none()
        .gap(px(8.0))
        .items_center()
        .px(px(6.0))
        .rounded(px(3.0))
        .hover(move |this| this.bg(hover_bg))
        .child(
            div()
                .flex_none()
                .w(px(70.0))
                .font_family(mono.clone())
                .text_xs()
                .text_color(accent)
                .child(short_hash.to_string()),
        )
        .child(
            div()
                .flex_none()
                .w(px(86.0))
                .font_family(mono.clone())
                .text_xs()
                .text_color(muted_fg)
                .child(e.selector.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(72.0))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(action_color)
                .child(e.action.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(e.subject.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(80.0))
                .text_xs()
                .text_color(muted_fg)
                .font_family(mono)
                .child(time_str),
        )
        .child(
            Button::new(SharedString::from(format!("vcs-reflog-checkout-{idx}")))
                .ghost()
                .xsmall()
                .icon(gpui_component::IconName::ArrowRight)
                .tooltip("Checkout 到此 commit（detached HEAD）")
                .disabled(busy)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.confirm_checkout_reflog(commit_for_btn.clone(), window, cx);
                })),
        )
        .into_any_element()
}

fn center(msg: &'static str, muted_fg: gpui::Hsla) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(muted_fg)
        .child(msg)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::CommitId;

    use super::*;

    fn reflog_entry() -> ReflogEntry {
        ReflogEntry {
            commit: CommitId("abcdef123456".into()),
            selector: "HEAD@{0}".into(),
            action: "checkout".into(),
            subject: "修复 ÜBER 问题".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn reflog_query_matches_fields_without_ascii_body_allocations() {
        let entry = reflog_entry();

        assert!(matches_reflog_query(&entry, "über"));
        assert!(matches_reflog_query(&entry, "checkout"));
        assert!(matches_reflog_query(&entry, "abcdef"));
        assert!(!matches_reflog_query(&entry, "missing"));
    }

    #[test]
    fn reflog_cache_requires_same_source_and_query() {
        let entries = Rc::new(vec![reflog_entry()]);
        let indices = Rc::new(vec![0]);
        let cache = ReflogRowsCacheEntry {
            entries: entries.clone(),
            query_lower: "checkout".into(),
            indices: indices.clone(),
        };

        let cached = cache.get(&entries, "checkout");
        assert!(
            cached
                .as_ref()
                .is_some_and(|value| Rc::ptr_eq(value, &indices))
        );
        assert!(cache.get(&entries, "commit").is_none());
        assert!(
            cache
                .get(&Rc::new(entries.as_ref().clone()), "checkout")
                .is_none()
        );
    }
}
