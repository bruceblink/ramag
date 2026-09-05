use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Selectable as _, Sizable as _, button::ButtonVariants as _, h_flex, input::Input,
    v_flex,
};
use ramag_domain::entities::{ClipKind, format_bytes};

use super::ClipboardView;
use crate::actions::{SelectNextClip, SelectPrevClip};

impl Render for ClipboardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_search_once {
            self.focused_search_once = true;
            self.search.update(cx, |state, cx| state.focus(window, cx));
        }
        if let Some(n) = self.pending_notification.take() {
            ramag_ui::push_responsive_notification(window, n, cx);
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
        format!("显示 {count} 条 · {usage} · 历史至少 500 条（仅加载前 500 条）")
    } else {
        format!("{count} 条 · 占用 {usage}")
    }
}

impl ClipboardView {
    /// 渲染剪贴板历史的搜索和类型筛选工具栏；两个区域在窄窗口中换行，保持控件可见。
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;

        ramag_ui::responsive_toolbar()
            .debug_selector(|| "clipboard-view-toolbar".into())
            .flex_none()
            .items_start()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .debug_selector(|| "clipboard-view-search-pane".into())
                    .flex_1()
                    .min_w(px(128.0))
                    .max_w(px(360.0))
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_r_1()
                    .border_color(border)
                    .child(
                        div()
                            .debug_selector(|| "clipboard-view-search".into())
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search).small()),
                    ),
            )
            .child(
                h_flex()
                    .debug_selector(|| "clipboard-view-filters".into())
                    .flex_1()
                    .min_w(px(128.0))
                    .items_center()
                    .px(px(12.0))
                    .py(px(8.0))
                    .child(self.render_filter_chips(cx)),
            )
    }

    fn render_filter_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = ramag_ui::responsive_toolbar()
            .debug_selector(|| "clipboard-filter-chips".into())
            .gap(px(4.0));

        row = row.child(
            ramag_ui::clickable_button("filter-all")
                .debug_selector(|| "clipboard-filter-all".into())
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
            let selector = format!("clipboard-filter-{}", kind.label());
            row = row.child(
                ramag_ui::clickable_button(SharedString::from(format!("filter-{}", kind.label())))
                    .debug_selector(move || selector.clone())
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
    fn on_select_next(&mut self, _: &SelectNextClip, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn on_select_prev(&mut self, _: &SelectPrevClip, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use gpui::{AppContext as _, Bounds, Pixels, TestAppContext, VisualTestContext, px, size};
    use ramag_app::ClipboardService;
    use ramag_domain::entities::{
        CapturedClip, ClipSource, ConnectionConfig, ConnectionId, QueryRecord, QueryRecordId,
    };
    use ramag_domain::error::Result;
    use ramag_domain::traits::{ClipboardDriver, Storage};

    use super::ClipboardView;
    use super::clipboard_status_label;

    struct TestClipboard;

    impl ClipboardDriver for TestClipboard {
        fn change_count(&self) -> i64 {
            0
        }

        fn own_change_count(&self) -> i64 {
            0
        }

        fn read(&self) -> Result<Option<CapturedClip>> {
            Ok(None)
        }

        fn write_text(&self, _: &str, _: Option<&[u8]>) -> Result<()> {
            Ok(())
        }

        fn write_image_png(&self, _: &[u8]) -> Result<()> {
            Ok(())
        }

        fn write_files(&self, _: &[String]) -> Result<()> {
            Ok(())
        }

        fn frontmost_app(&self) -> Option<ClipSource> {
            None
        }

        fn app_icon_png(&self, _: &str) -> Option<Arc<Vec<u8>>> {
            None
        }

        fn persist_media(&self, key: &str, _: &[u8]) -> Result<String> {
            Ok(key.to_string())
        }

        fn read_media(&self, _: &str) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn list_media(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn remove_media(&self, _: &str) -> Result<()> {
            Ok(())
        }

        fn clear_media(&self) -> Result<()> {
            Ok(())
        }

        fn accessibility_trusted(&self, _: bool) -> bool {
            false
        }

        fn paste_to_app(&self, _: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn open_url(&self, _: &str) -> Result<()> {
            Ok(())
        }

        fn reveal_in_file_manager(&self, _: &[String]) -> Result<()> {
            Ok(())
        }

        fn paths_exist(&self, _: &[String]) -> bool {
            false
        }
    }

    struct TestStorage;

    #[async_trait]
    impl Storage for TestStorage {
        async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
            Ok(Vec::new())
        }

        async fn get_connection(&self, _: &ConnectionId) -> Result<Option<ConnectionConfig>> {
            Ok(None)
        }

        async fn save_connection(&self, _: &ConnectionConfig) -> Result<()> {
            Ok(())
        }

        async fn delete_connection(&self, _: &ConnectionId) -> Result<()> {
            Ok(())
        }

        async fn append_history(&self, _: &QueryRecord) -> Result<()> {
            Ok(())
        }

        async fn list_history(
            &self,
            _: Option<&ConnectionId>,
            _: usize,
        ) -> Result<Vec<QueryRecord>> {
            Ok(Vec::new())
        }

        async fn delete_history(&self, _: &QueryRecordId) -> Result<()> {
            Ok(())
        }

        async fn clear_history(&self, _: Option<&ConnectionId>) -> Result<()> {
            Ok(())
        }

        async fn get_preference(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }

        async fn set_preference(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn add_clipboard_window(cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.update(gpui_component::init);
        let (_, visual_cx) = cx.add_window_view(|window, cx| {
            let service = Arc::new(ClipboardService::new(
                Arc::new(TestClipboard),
                Arc::new(TestStorage),
            ));
            let entity = cx.new(|cx| ClipboardView::new(service, window, cx));
            gpui_component::Root::new(entity, window, cx)
        });
        visual_cx
    }

    fn assert_inside(parent: &Bounds<Pixels>, child: &Bounds<Pixels>, label: &str) {
        assert!(
            child.origin.x >= parent.origin.x
                && child.origin.y >= parent.origin.y
                && child.right() <= parent.right()
                && child.bottom() <= parent.bottom(),
            "{label} 越出父容器：parent={parent:?}, child={child:?}"
        );
    }

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
            "显示 500 条 · 2 KiB · 历史至少 500 条（仅加载前 500 条）"
        );
    }

    /// 搜索框和类型筛选在主页面工具栏中应随窗口宽度换行，且每个按钮都留在筛选区内。
    #[gpui::test]
    fn clipboard_toolbar_wraps_search_and_filters_inside_supported_widths(cx: &mut TestAppContext) {
        let cx = add_clipboard_window(cx);

        for width in [180.0, 240.0, 360.0, 600.0, 1024.0, 1440.0] {
            cx.simulate_resize(size(px(width), px(480.0)));
            cx.run_until_parked();

            let toolbar = cx.debug_bounds("clipboard-view-toolbar");
            assert!(toolbar.is_some(), "剪贴板主页面工具栏应渲染");
            let Some(toolbar) = toolbar else { continue };
            let search_pane = cx.debug_bounds("clipboard-view-search-pane");
            assert!(search_pane.is_some(), "剪贴板搜索区应渲染");
            let Some(search_pane) = search_pane else {
                continue;
            };
            let search = cx.debug_bounds("clipboard-view-search");
            assert!(search.is_some(), "剪贴板搜索框应渲染");
            let Some(search) = search else { continue };
            let filters = cx.debug_bounds("clipboard-view-filters");
            assert!(filters.is_some(), "剪贴板筛选区应渲染");
            let Some(filters) = filters else { continue };

            assert_inside(&toolbar, &search_pane, "搜索区");
            assert_inside(&search_pane, &search, "搜索框");
            assert_inside(&toolbar, &filters, "筛选区");
            for selector in [
                "clipboard-filter-all",
                "clipboard-filter-文本",
                "clipboard-filter-链接",
                "clipboard-filter-颜色",
                "clipboard-filter-图片",
                "clipboard-filter-文件",
            ] {
                let button = cx.debug_bounds(selector);
                assert!(button.is_some(), "{selector} 应渲染");
                if let Some(button) = button {
                    assert_inside(&filters, &button, selector);
                }
            }

            if width <= 240.0 {
                assert!(
                    filters.origin.y > search_pane.origin.y,
                    "窄窗口应让筛选区换到搜索区下方：search={search_pane:?}, filters={filters:?}"
                );
            }
        }
    }
}
