//! 结果单元格查看器：只展示当前选中的值，并对显示内容设置字节上限。

use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Render, ScrollHandle, Styled,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};
use ramag_domain::entities::{DriverKind, QueryResult, Value};

use super::result_panel::ResultPanel;
use super::result_value::{CellCopyFormat, cell_size_summary, cell_status, format_cell_value};

const VALUE_VIEW_WIDTH: f32 = 960.0;
// 留出标题、复制工具栏和底部操作区的空间，让小窗口也能完整显示对话框。
const VALUE_VIEW_HEIGHT: f32 = 400.0;
/// 单个值的查看上限；结果集本身仍由已有的全局结果预算控制。
const MAX_VALUE_VIEW_BYTES: usize = 2 * 1024 * 1024;

fn responsive_dialog_width(window: &Window, preferred: f32) -> gpui::Pixels {
    let available = f32::from(window.viewport_size().width);
    px((available - 32.0).max(160.0).min(preferred))
}

pub(crate) struct ResultValueDialog {
    result: Arc<QueryResult>,
    row_index: usize,
    column_index: usize,
    driver: DriverKind,
    value_override: Option<Value>,
    vertical_scroll: ScrollHandle,
    horizontal_scroll: ScrollHandle,
}

impl ResultValueDialog {
    fn new(
        result: Arc<QueryResult>,
        row_index: usize,
        column_index: usize,
        driver: DriverKind,
        value_override: Option<Value>,
    ) -> Self {
        Self {
            result,
            row_index,
            column_index,
            driver,
            value_override,
            vertical_scroll: ScrollHandle::new(),
            horizontal_scroll: ScrollHandle::new(),
        }
    }

    fn value(&self) -> Option<&Value> {
        self.value_override.as_ref().or_else(|| {
            self.result
                .rows
                .get(self.row_index)
                .and_then(|row| row.values.get(self.column_index))
        })
    }

    /// 生成当前值的有界正文；长文本不会在打开查看器时复制整个结果集。
    fn bounded_value_text(&self) -> (String, bool) {
        match self.value() {
            None => ("缺失".to_string(), false),
            Some(Value::Null) => ("NULL".to_string(), false),
            Some(value) => value.display_for_edit_bounded(MAX_VALUE_VIEW_BYTES),
        }
    }

    /// 创建一种复制格式按钮，回调只读取当前行列，不读取其他结果数据。
    fn copy_button(&self, format: CellCopyFormat) -> impl IntoElement {
        let result = self.result.clone();
        let row_index = self.row_index;
        let column_index = self.column_index;
        let driver = self.driver;
        let value_override = self.value_override.clone();
        ramag_ui::clickable_button(format!("result-value-copy-{:?}", format))
            .ghost()
            .small()
            .icon(IconName::Copy)
            .label(format.label())
            .on_click(move |_: &ClickEvent, window, app| {
                let value = value_override.as_ref().or_else(|| {
                    result
                        .rows
                        .get(row_index)
                        .and_then(|row| row.values.get(column_index))
                });
                let text = format_cell_value(value, format, driver);
                ramag_ui::copy_text_with_notification(text, window, app);
            })
    }

    fn render_scrollable(
        &self,
        body: impl IntoElement,
        theme: &gpui_component::Theme,
    ) -> AnyElement {
        div()
            .id("result-value-viewer-scroll-area")
            .debug_selector(|| "result-value-viewer-scroll-area".into())
            .relative()
            .h(px(VALUE_VIEW_HEIGHT))
            .w_full()
            .min_w_0()
            .child(
                div()
                    .id("result-value-viewer-horizontal-scroll")
                    .size_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .track_scroll(&self.horizontal_scroll)
                    .child(
                        div()
                            .id("result-value-viewer-vertical-scroll")
                            .w(px(VALUE_VIEW_WIDTH))
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.vertical_scroll)
                            .child(body),
                    ),
            )
            .child(
                div()
                    .id("result-value-viewer-v-scrollbar")
                    .debug_selector(|| "result-value-viewer-v-scrollbar".into())
                    .absolute()
                    .top_0()
                    .bottom(px(16.0))
                    .right_0()
                    .w(px(16.0))
                    .bg(theme.scrollbar)
                    .child(
                        Scrollbar::vertical(&self.vertical_scroll)
                            .id("result-value-viewer-v-scrollbar-control")
                            .scrollbar_show(ScrollbarShow::Always),
                    ),
            )
            .child(
                div()
                    .id("result-value-viewer-h-scrollbar")
                    .debug_selector(|| "result-value-viewer-h-scrollbar".into())
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(16.0))
                    .bg(theme.scrollbar)
                    .child(
                        Scrollbar::horizontal(&self.horizontal_scroll)
                            .id("result-value-viewer-h-scrollbar-control")
                            .scroll_size(gpui::size(px(VALUE_VIEW_WIDTH), px(VALUE_VIEW_HEIGHT))),
                    ),
            )
            .into_any_element()
    }
}

/// Opens the value viewer for a stable row/column coordinate and an optional staged value.
pub(crate) fn open(
    result: Arc<QueryResult>,
    row_index: usize,
    column_index: usize,
    driver: DriverKind,
    value_override: Option<Value>,
    window: &mut Window,
    cx: &mut Context<ResultPanel>,
) {
    let Some(column_name) = result.columns.get(column_index).cloned() else {
        return;
    };
    let title = format!("查看 行 {} · {}", row_index + 1, column_name);
    let viewer =
        cx.new(|_| ResultValueDialog::new(result, row_index, column_index, driver, value_override));
    let viewer_for_dialog = viewer.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        let viewer_for_content = viewer_for_dialog.clone();
        let dialog_width = responsive_dialog_width(window, 1040.0);
        let close = div()
            .debug_selector(|| "result-value-viewer-close".into())
            .child(
                ramag_ui::clickable_button("result-value-viewer-close-button")
                    .ghost()
                    .small()
                    .label("关闭")
                    .on_click(|_: &ClickEvent, window, app| window.close_dialog(app)),
            );
        dialog
            .title(ramag_ui::closable_dialog_title(
                "result-value-viewer-close-title",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .width(dialog_width)
            .margin_top(px(36.0))
            .content(move |content, _, _| content.child(viewer_for_content.clone()))
            .footer(h_flex().w_full().items_center().justify_end().child(close))
    });
}

impl Render for ResultValueDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let column_name = self
            .result
            .columns
            .get(self.column_index)
            .cloned()
            .unwrap_or_else(|| "未知列".to_string());
        let type_name = self
            .result
            .column_types
            .get(self.column_index)
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| "未知类型".to_string());
        let (value_text, truncated) = self.bounded_value_text();
        let status = cell_status(self.value());
        let size = cell_size_summary(self.value());

        let mut meta = h_flex()
            .id("result-value-viewer-meta")
            .debug_selector(|| "result-value-viewer-meta".into())
            .w_full()
            .min_w_0()
            .flex_none()
            .items_center()
            .flex_wrap()
            .gap(px(14.0))
            .pb(px(8.0))
            .text_xs()
            .child(div().text_color(theme.foreground).child(column_name))
            .child(div().text_color(theme.muted_foreground).child(type_name))
            .child(div().text_color(theme.muted_foreground).child(status))
            .child(div().text_color(theme.muted_foreground).child(size));
        if truncated {
            meta = meta.child(div().text_color(theme.warning).child(format!(
                "仅显示前 {} MiB",
                MAX_VALUE_VIEW_BYTES / 1024 / 1024
            )));
        }

        let copy_toolbar = h_flex()
            .id("result-value-viewer-copy-toolbar")
            .debug_selector(|| "result-value-viewer-copy-toolbar".into())
            .w_full()
            .min_w_0()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .pb(px(8.0))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("复制为"),
            )
            .children(
                CellCopyFormat::ALL
                    .into_iter()
                    .map(|format| self.copy_button(format)),
            );

        let content = div()
            .id("result-value-viewer-content-frame")
            .debug_selector(|| "result-value-viewer-content-frame".into())
            .w(px(VALUE_VIEW_WIDTH))
            .min_h(px(VALUE_VIEW_HEIGHT))
            .p(px(12.0))
            .bg(theme.background)
            .font_family(theme.mono_font_family.clone())
            .child(
                ramag_ui::SelectableText::new("result-value-viewer-content", value_text)
                    .text_color(theme.foreground),
            );

        v_flex()
            .id("result-value-viewer")
            .debug_selector(|| "result-value-viewer".into())
            .w_full()
            .min_w_0()
            .gap(px(2.0))
            .child(meta)
            .child(copy_toolbar)
            .child(self.render_scrollable(content, theme))
    }
}
