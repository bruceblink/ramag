use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Selectable as _, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    h_flex, input::Input, v_flex,
};
use ramag_domain::entities::{ClipKind, format_bytes};

use super::ClipboardView;
use crate::actions::{
    CopySelectedClip, DeleteSelectedClip, FocusClipSearch, SelectNextClip, SelectPrevClip,
};

impl Render for ClipboardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_search_once {
            self.focused_search_once = true;
            self.search.update(cx, |state, cx| state.focus(window, cx));
        }
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }

        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let visible = self.visible_items(cx);
        let count = visible.len();
        let total_bytes = visible
            .iter()
            .fold(0_u64, |total, item| total.saturating_add(item.byte_size));
        let query_active = !self.search.read(cx).value().trim().is_empty();
        let count_label =
            clipboard_status_label(count, total_bytes, query_active && self.search_truncated);
        let focus = self.focus_handle.clone();

        v_flex()
            .key_context("ClipboardView")
            .track_focus(&focus)
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_copy_selected))
            .on_action(cx.listener(Self::on_delete_selected))
            .on_action(cx.listener(Self::on_select_next))
            .on_action(cx.listener(Self::on_select_prev))
            .size_full()
            .child(self.render_toolbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w(px(360.0))
                            .h_full()
                            .border_r_1()
                            .border_color(border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .child(self.render_list(visible, cx)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w_full()
                                    .px(px(12.0))
                                    .py(px(6.0))
                                    .border_t_1()
                                    .border_color(border)
                                    .text_xs()
                                    .text_color(muted)
                                    .child(count_label),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_detail(cx)),
                    ),
            )
    }
}

fn clipboard_status_label(count: usize, total_bytes: u64, search_truncated: bool) -> String {
    let usage = format_bytes(total_bytes);
    if search_truncated {
        format!("显示 {count} 条 · 占用 {usage} · 历史匹配至少 500 条，仅加载前 500 条")
    } else {
        format!("{count} 条 · 占用 {usage}")
    }
}

impl ClipboardView {
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;

        h_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .w(px(360.0))
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_r_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search).small()),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .px(px(12.0))
                    .py(px(8.0))
                    .child(self.render_filter_chips(cx)),
            )
    }

    fn render_filter_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().items_center().gap(px(4.0));

        row = row.child(
            ramag_ui::clickable_button("filter-all")
                .ghost()
                .xsmall()
                .label("全部")
                .selected(self.filter.is_none())
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.filter = None;
                    cx.notify();
                })),
        );
        for &kind in ClipKind::all() {
            let active = self.filter == Some(kind);
            row = row.child(
                ramag_ui::clickable_button(SharedString::from(format!("filter-{}", kind.label())))
                    .ghost()
                    .xsmall()
                    .label(kind.label())
                    .selected(active)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.filter = Some(kind);
                        cx.notify();
                    })),
            );
        }
        row
    }

    fn render_list(
        &self,
        visible: Vec<std::sync::Arc<ramag_domain::entities::ClipItem>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if visible.is_empty() {
            let muted = cx.theme().muted_foreground;
            let query = self.search.read(cx).value().trim().to_string();
            let hint = if !query.is_empty() {
                format!("没有匹配「{query}」的条目")
            } else if self.filter.is_some() {
                "该类型下暂无条目".to_string()
            } else if !self.settings.enabled {
                "采集已关闭：请在“设置 > 剪贴板”中开启后再使用".to_string()
            } else {
                "暂无剪贴历史；复制任意内容后会出现在这里".to_string()
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(hint)
                .into_any_element();
        }

        let count = visible.len();
        let entity = cx.entity().clone();
        uniform_list("clip-list", count, move |range, _window, cx| {
            range
                .map(|ix| {
                    entity.update(cx, |this, cx| {
                        this.render_card(visible[ix].clone(), cx).into_any_element()
                    })
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.list_scroll)
        .size_full()
        .into_any_element()
    }
}

impl ClipboardView {
    fn on_focus_search(
        &mut self,
        _: &FocusClipSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn on_copy_selected(&mut self, _: &CopySelectedClip, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected(cx);
    }

    fn on_delete_selected(
        &mut self,
        _: &DeleteSelectedClip,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_selected(window, cx);
    }

    fn on_select_next(&mut self, _: &SelectNextClip, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn on_select_prev(&mut self, _: &SelectPrevClip, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::clipboard_status_label;

    #[test]
    fn clipboard_status_includes_readable_content_size() {
        assert_eq!(
            clipboard_status_label(257, 18 * 1024 * 1024, false),
            "257 条 · 占用 18.0 MiB"
        );
    }

    #[test]
    fn truncated_search_status_keeps_size_and_limit_hint() {
        assert_eq!(
            clipboard_status_label(500, 2048, true),
            "显示 500 条 · 占用 2 KiB · 历史匹配至少 500 条，仅加载前 500 条"
        );
    }
}
