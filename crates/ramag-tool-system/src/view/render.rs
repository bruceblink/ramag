//! 系统监控视图的布局、指标卡片和任务列表渲染。

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, v_flex,
};

use super::helpers::{
    core_grid_dimensions, empty_state, format_bytes, format_percent, format_rate_pair, history_max,
    metric_card, panel_heading, process_header, process_table_layout, ratio_percent,
    render_core_grid, render_disk_row, render_history, render_meter_row,
};
use super::{Notice, SystemSection, SystemView, TerminationRequest};
use crate::{MAX_VISIBLE_PROCESSES, MonitorSnapshot, ProcessSort};

impl SystemView {
    pub(super) fn section_button(
        &self,
        section: SystemSection,
        label: &'static str,
        icon: impl Into<Icon>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut button =
            ramag_ui::clickable_button(SharedString::from(format!("system-section-{label}")))
                .small()
                .icon(icon)
                .label(label);
        button = if self.section == section {
            button.primary()
        } else {
            button.ghost()
        };
        button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.select_section(section, cx);
        }))
    }

    fn render_notice(&self, notice: &Notice, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let color = if notice.error {
            theme.danger
        } else {
            theme.success
        };
        let mut background = color;
        background.a = 0.12;
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px_4()
            .py(px(7.0))
            .bg(background)
            .text_xs()
            .text_color(color)
            .child(Icon::new(if notice.error {
                IconName::CircleX
            } else {
                IconName::CircleCheck
            }))
            .child(notice.message.clone())
    }

    fn render_termination_confirmation(
        &self,
        request: &TerminationRequest,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let mut background = theme.danger;
        background.a = 0.12;
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .px_4()
            .py(px(8.0))
            .bg(background)
            .text_xs()
            .text_color(theme.danger)
            .child(format!(
                "确认终止进程 {}（PID {}）？",
                request.name, request.pid
            ))
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(6.0))
                    .child(
                        ramag_ui::clickable_button("system-kill-cancel")
                            .ghost()
                            .xsmall()
                            .label("取消")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cancel_termination(cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("system-kill-confirm")
                            .danger()
                            .xsmall()
                            .label("终止")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.confirm_termination(cx);
                            })),
                    ),
            )
    }

    fn render_performance(&self, snapshot: &MonitorSnapshot, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let memory_percent = ratio_percent(snapshot.memory_used, snapshot.memory_total);
        let swap_percent = ratio_percent(snapshot.swap_used, snapshot.swap_total);
        let mut overview = h_flex().w_full().min_w_0().flex().flex_wrap().gap(px(10.0));
        overview = overview.child(metric_card(
            "CPU",
            format_percent(snapshot.cpu_percent as f64),
            format!("{} 个核心", snapshot.core_usages.len()),
            ramag_ui::icons::gauge(),
            theme.accent,
            theme,
        ));
        overview = overview.child(metric_card(
            "内存",
            format_percent(memory_percent),
            format!(
                "{} / {}",
                format_bytes(snapshot.memory_used),
                format_bytes(snapshot.memory_total)
            ),
            Icon::new(IconName::MemoryStick),
            theme.info,
            theme,
        ));
        overview = overview.child(metric_card(
            "交换空间",
            format_percent(swap_percent),
            format!(
                "{} / {}",
                format_bytes(snapshot.swap_used),
                format_bytes(snapshot.swap_total)
            ),
            Icon::new(IconName::ArrowDown),
            theme.warning,
            theme,
        ));
        overview = overview.child(metric_card(
            "磁盘 I/O",
            format_rate_pair(
                "读",
                snapshot.disk_read_rate_mb,
                "写",
                snapshot.disk_write_rate_mb,
            ),
            "所有进程累计速率".to_owned(),
            Icon::new(IconName::HardDrive),
            theme.success,
            theme,
        ));
        overview = overview.child(metric_card(
            "网络",
            format_rate_pair(
                "收",
                snapshot.network_received_rate_mb,
                "发",
                snapshot.network_transmitted_rate_mb,
            ),
            "所有接口累计速率".to_owned(),
            Icon::new(IconName::Network),
            theme.secondary_foreground,
            theme,
        ));

        let (core_columns, core_rows) = core_grid_dimensions(snapshot.core_usages.len());
        let cores = v_flex()
            .debug_selector(|| "system-core-panel".to_owned())
            .flex_1()
            .min_w(px(300.0))
            .h(px(260.0))
            .gap(px(6.0))
            .p(px(12.0))
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(panel_heading(
                "CPU 核心",
                format!(
                    "{} 个核心 · {} 列 × {} 行",
                    snapshot.core_usages.len(),
                    core_columns,
                    core_rows
                ),
                theme,
            ))
            .child(render_core_grid(
                &snapshot.core_usages,
                &snapshot.core_histories,
                theme,
            ));

        let mut memory_panel = v_flex()
            .flex_1()
            .min_w(px(300.0))
            .h(px(260.0))
            .gap(px(8.0))
            .p(px(12.0))
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(panel_heading("内存与交换空间", "当前占用", theme));
        memory_panel = memory_panel.child(render_meter_row(
            "内存",
            memory_percent,
            format!(
                "{} / {}",
                format_bytes(snapshot.memory_used),
                format_bytes(snapshot.memory_total)
            ),
            theme.info,
            theme,
        ));
        memory_panel = memory_panel.child(render_meter_row(
            "交换",
            swap_percent,
            format!(
                "{} / {}",
                format_bytes(snapshot.swap_used),
                format_bytes(snapshot.swap_total)
            ),
            theme.warning,
            theme,
        ));
        memory_panel = memory_panel.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("采集时长 {:.0}s", snapshot.elapsed_seconds)),
        );

        let mut history_row = h_flex().w_full().flex().flex_wrap().gap(px(10.0));
        history_row = history_row.child(render_history(
            "CPU 使用率",
            &snapshot.cpu_history,
            100.0,
            "%",
            theme.accent,
            theme,
        ));
        history_row = history_row.child(render_history(
            "内存使用率",
            &snapshot.memory_history,
            100.0,
            "%",
            theme.info,
            theme,
        ));
        history_row = history_row.child(render_history(
            "网络接收",
            &snapshot.network_received_history,
            history_max(&snapshot.network_received_history),
            " MB/s",
            theme.secondary_foreground,
            theme,
        ));
        history_row = history_row.child(render_history(
            "磁盘读取",
            &snapshot.disk_read_history,
            history_max(&snapshot.disk_read_history),
            " MB/s",
            theme.success,
            theme,
        ));

        let mut disk_panel = v_flex()
            .w_full()
            .gap(px(6.0))
            .p(px(12.0))
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(panel_heading("磁盘空间", "挂载点", theme));
        if snapshot.disks.is_empty() {
            disk_panel = disk_panel.child(empty_state("暂时没有磁盘数据", theme));
        } else {
            for disk in &snapshot.disks {
                disk_panel = disk_panel.child(render_disk_row(disk, theme));
            }
        }

        let mut body = v_flex()
            .debug_selector(|| "system-performance-body".to_owned())
            .w_full()
            .min_w_0()
            .gap(px(10.0))
            .p(px(16.0))
            .child(overview);
        if let Some(warning) = &snapshot.data_warning {
            let mut background = theme.warning;
            background.a = 0.12;
            body = body.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .px_3()
                    .py(px(7.0))
                    .bg(background)
                    .text_xs()
                    .text_color(theme.warning)
                    .child(Icon::new(IconName::TriangleAlert))
                    .child(warning.clone()),
            );
        }
        body.child(
            h_flex()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap(px(10.0))
                .children([cores.into_any_element(), memory_panel.into_any_element()]),
        )
        .child(history_row)
        .child(disk_panel)
        .into_any_element()
    }

    /// 根据窗口宽度切换进程表列尺寸，所有字段保持可见而长进程名可在窄窗口省略。
    fn render_processes(
        &self,
        snapshot: &MonitorSnapshot,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let layout = process_table_layout(window.viewport_size().width < px(560.0));
        let current_pid = std::process::id();
        let mut sort_buttons = h_flex().flex_wrap().gap(px(4.0));
        for sort in [ProcessSort::Cpu, ProcessSort::Memory] {
            let selected = self.monitor.process_sort() == sort;
            let mut button = ramag_ui::clickable_button(SharedString::from(format!(
                "system-process-sort-{}",
                sort.label()
            )))
            .xsmall()
            .label(format!("按 {} 排序", sort.label()));
            button = if selected {
                button.primary()
            } else {
                button.ghost()
            };
            sort_buttons = sort_buttons.child(button.on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.select_process_sort(sort, cx)),
            ));
        }

        let mut table = v_flex()
            .debug_selector(|| "system-process-table".to_owned())
            .w_full()
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0));
        table = table.child(process_header(layout, theme));
        if snapshot.processes.is_empty() {
            table = table.child(empty_state("暂时没有进程数据", theme));
        } else {
            for process in snapshot.processes.iter().take(MAX_VISIBLE_PROCESSES) {
                let pid = process.pid;
                let name = process.name.clone();
                let is_current = pid == current_pid;
                let mut terminate =
                    ramag_ui::clickable_button(SharedString::from(format!("system-kill-{pid}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::CircleX)
                        .tooltip(if is_current {
                            "不能终止当前 Ramag 进程"
                        } else {
                            "终止进程"
                        })
                        .disabled(is_current || self.termination_in_progress);
                if !is_current && !self.termination_in_progress {
                    let name_for_click = name.clone();
                    terminate =
                        terminate.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.request_termination(pid, name_for_click.clone(), cx);
                        }));
                }
                table = table.child(
                    h_flex()
                        .w_full()
                        .min_h(px(34.0))
                        .items_center()
                        .gap(px(layout.gap))
                        .px(px(layout.horizontal_padding))
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .w(px(layout.pid_width))
                                .text_xs()
                                .child(pid.to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(layout.process_name_min_width))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .child(name),
                        )
                        .child(
                            div()
                                .w(px(layout.cpu_width))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format_percent(process.cpu_percent as f64)),
                        )
                        .child(
                            div()
                                .w(px(layout.memory_width))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format_bytes(process.memory_bytes)),
                        )
                        .child(div().w(px(layout.action_width)).child(terminate)),
                );
            }
        }

        v_flex()
            .w_full()
            .gap(px(10.0))
            .p(px(16.0))
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("任务管理器"),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                format!(
                                    "{} 个进程，最多显示 {} 行",
                                    snapshot.processes.len(),
                                    MAX_VISIBLE_PROCESSES
                                ),
                            )),
                    )
                    .child(sort_buttons),
            )
            .child(table)
            .into_any_element()
    }
}

impl Render for SystemView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.monitor.snapshot();
        let mut root = v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        root = root.child(self.render_header(window, cx));
        if let Some(notice) = &self.notice {
            root = root.child(self.render_notice(notice, cx));
        }
        if let Some(request) = &self.termination_request {
            root = root.child(self.render_termination_confirmation(request, cx));
        }
        let content = match self.section {
            SystemSection::Performance => self.render_performance(&snapshot, cx),
            SystemSection::Processes => self.render_processes(&snapshot, window, cx),
        };
        root.child(
            div()
                .id("system-content")
                .debug_selector(|| "system-content".to_owned())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(content),
        )
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
