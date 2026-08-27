//! SQL 结果面板渲染与复制。

use gpui::{
    ClickEvent, ClipboardItem, Context, Focusable as _, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::*,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex,
    v_flex,
};
use ramag_ui::platform::primary_shortcut;

use super::ResultPanel;
use super::ResultState;
use crate::actions::{
    CopyCellAsCsv, CopyCellAsJson, CopyCellAsSql, CopyCellValue, CopySelectedColumn, FindInResults,
    OpenCellValueViewer,
};
use crate::views::result_table::render_result_view;
use crate::views::result_value::{CellCopyFormat, format_cell_value};

impl Render for ResultPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 把异步任务挂的通知 push 到全局 toast
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }

        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let secondary_bg = theme.secondary;
        let danger = theme.danger;
        let muted_bg = theme.muted;
        let accent = theme.accent;

        // Ok 仅克隆 Arc，避免借用 state 时无法启动异步派生视图任务。
        let state = self.state.clone();
        let content = match state {
            ResultState::Empty => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .text_color(muted_fg)
                .text_xs()
                .child("点左侧表名查看数据")
                .child(format!(
                    "或按 {} 唤出 SQL 编辑器，再按 {} 运行",
                    primary_shortcut("E"),
                    primary_shortcut("Enter")
                ))
                .into_any_element(),

            ResultState::Running => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(muted_fg)
                .text_xs()
                .child("执行中…")
                .into_any_element(),

            ResultState::Error(msg) => {
                let msg_for_copy = msg.clone();
                v_flex()
                    .size_full()
                    .p_4()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(danger)
                                    .child("执行失败"),
                            )
                            .child(div().flex_1())
                            .child(
                                ramag_ui::clickable_button("copy-error")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Copy)
                                    .tooltip("复制")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            msg_for_copy.clone(),
                                        ));
                                        this.pending_notification =
                                            Some(ramag_ui::copy_success_notification());
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(div().text_xs().text_color(fg).child(msg))
                    .into_any_element()
            }

            ResultState::Released(msg) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .px_4()
                .text_xs()
                .text_color(theme.warning)
                .child(msg)
                .into_any_element(),

            ResultState::Ok(result) if self.plan.enabled => super::plan::render_plan(
                self,
                &result,
                fg,
                muted_fg,
                secondary_bg,
                border,
                muted_bg,
                accent,
                cx,
            ),

            ResultState::Ok(result) => render_result_view(self, &result, cx),
        };

        let warnings_banner = self.render_warnings_banner(cx);

        let mut root = v_flex()
            .size_full()
            .min_w_0()
            .on_action(cx.listener(|this, _: &FindInResults, window, cx| {
                let handle = this.row_filter_input.read(cx).focus_handle(cx);
                handle.focus(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CopyCellValue, _, cx| {
                this.copy_selected_cell(cx);
            }))
            .on_action(cx.listener(|this, _: &CopySelectedColumn, _, cx| {
                this.copy_selected_column_name(cx);
            }))
            .on_action(cx.listener(|this, _: &CopyCellAsCsv, _, cx| {
                this.copy_selected_cell_as(CellCopyFormat::Csv, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyCellAsJson, _, cx| {
                this.copy_selected_cell_as(CellCopyFormat::Json, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyCellAsSql, _, cx| {
                this.copy_selected_cell_as(CellCopyFormat::Sql, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenCellValueViewer, window, cx| {
                this.open_selected_cell_viewer(window, cx);
            }));
        if let Some(banner) = warnings_banner {
            root = root.child(banner);
        }
        root.child(div().flex_1().min_h_0().child(content))
    }
}

impl ResultPanel {
    /// 渲染 SHOW WARNINGS 提示条
    pub(super) fn render_warnings_banner(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let ResultState::Ok(qr) = &self.state else {
            return None;
        };
        if qr.warnings.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let warning_color = theme.warning;
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let secondary_bg = theme.secondary;

        let count = qr.warnings.len();
        let expanded = self.warnings_expanded;
        let header_label = if expanded {
            format!("⚠ {count} 条查询警告（点击收起）")
        } else {
            format!("⚠ {count} 条查询警告（点击展开）")
        };
        let header = h_flex()
            .id(SharedString::from("warnings-header"))
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .bg(secondary_bg)
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(warning_color)
                    .child(header_label),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.warnings_expanded = !this.warnings_expanded;
                cx.notify();
            }));

        if !expanded {
            return Some(header.into_any_element());
        }

        const MAX_VISIBLE: usize = 20;
        let mut rows: Vec<gpui::AnyElement> =
            Vec::with_capacity(qr.warnings.len().min(MAX_VISIBLE) + 1);
        for w in qr.warnings.iter().take(MAX_VISIBLE) {
            let line = format!("[{} {}] {}", w.level, w.code, w.message);
            rows.push(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(fg)
                    .child(line)
                    .into_any_element(),
            );
        }
        if count > MAX_VISIBLE {
            rows.push(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("…更多 {} 条", count - MAX_VISIBLE))
                    .into_any_element(),
            );
        }

        Some(
            v_flex()
                .w_full()
                .flex_none()
                .border_b_1()
                .border_color(border)
                .child(header)
                .child(v_flex().py_1().children(rows))
                .into_any_element(),
        )
    }

    /// 复制选中单元格完整值
    pub(crate) fn copy_selected_cell(&mut self, cx: &mut Context<Self>) {
        self.copy_selected_cell_as(CellCopyFormat::Text, cx);
    }

    /// 按指定格式复制选中单元格，只处理当前行列而不遍历整个结果集。
    pub(crate) fn copy_selected_cell_as(&mut self, format: CellCopyFormat, cx: &mut Context<Self>) {
        let Some((ri, ci)) = self.selected_cell else {
            return;
        };
        let ResultState::Ok(result) = &self.state else {
            return;
        };
        let driver = self
            .connection
            .as_ref()
            .map(|connection| connection.driver)
            .unwrap_or(ramag_domain::entities::DriverKind::Mysql);
        let text = format_cell_value(
            result.rows.get(ri).and_then(|row| row.values.get(ci)),
            format,
            driver,
        );
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.pending_notification = Some(ramag_ui::copy_success_notification());
        cx.notify();
    }

    /// 打开当前单元格的有界查看器；查看动作只保留结果 Arc 和源坐标。
    pub(crate) fn open_selected_cell_viewer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((ri, ci)) = self.selected_cell else {
            return;
        };
        let ResultState::Ok(result) = &self.state else {
            return;
        };
        let driver = self
            .connection
            .as_ref()
            .map(|connection| connection.driver)
            .unwrap_or(ramag_domain::entities::DriverKind::Mysql);
        crate::views::result_value_dialog::open(result.clone(), ri, ci, driver, window, cx);
    }

    /// 复制选中列的列名
    pub(super) fn copy_selected_column_name(&mut self, cx: &mut Context<Self>) {
        let Some((_, ci)) = self.selected_cell else {
            return;
        };
        let ResultState::Ok(result) = &self.state else {
            return;
        };
        let Some(name) = result.columns.get(ci).cloned() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
        self.pending_notification = Some(ramag_ui::copy_success_notification());
        cx.notify();
    }
}
