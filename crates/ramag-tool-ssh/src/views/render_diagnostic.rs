//! 生产安全诊断工作区渲染。

use gpui::{ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex,
    scroll::ScrollableElement as _, v_flex,
};
use ramag_domain::entities::{
    RemoteCapabilityState, SshDiagnosticOperation, SshLogSource, SshProfileId,
};

use super::SshView;
use super::ops_diagnostic::filter_diagnostic_output;

impl SshView {
    pub(super) fn render_diagnostic_pane(
        &self,
        workspace_id: SshProfileId,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &workspace_id)
        else {
            return div().into_any_element();
        };
        let profile = workspace.profile.clone();
        let capabilities = workspace.capabilities.clone();
        let capability_loading = workspace.capability_loading;
        let capability_error = workspace.capability_error.clone();
        let diagnostic_loading = workspace.diagnostic_loading;
        let diagnostic_error = workspace.diagnostic_error.clone();
        let diagnostic_result = workspace.diagnostic_result.clone();
        let diagnostic_query = workspace.diagnostic_query.clone();
        let selected_path = workspace.selected_path.clone();
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let production_badge = h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .justify_between()
            .px(px(14.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(border)
            .child(
                v_flex()
                    .gap(px(3.0))
                    .child(div().text_sm().child("生产 · 低影响只读诊断"))
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{} · {}@{}",
                        profile.name, profile.username, profile.host
                    ))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child("禁止完整 Shell、上传、编辑、重命名和删除"),
            );
        let status = if capability_loading {
            "能力探测中…".to_string()
        } else if let Some(error) = capability_error {
            format!("能力探测失败：{error}")
        } else if let Some(capabilities) = capabilities.as_ref() {
            format!(
                "OpenSSH {:?} · 认证 {:?} · 执行 {:?} · Terminal {:?} · SFTP {:?} · 诊断 {:?} · 远端 {:?} · 命名空间 {:?}",
                capabilities.openssh_client,
                capabilities.ssh_authentication,
                capabilities.ssh_execution,
                capabilities.terminal,
                capabilities.sftp,
                capabilities.diagnostic,
                capabilities.operating_system,
                capabilities.sftp_namespace
            )
        } else {
            "尚未探测远端能力".into()
        };
        let available = capabilities.as_ref().is_some_and(|capabilities| {
            capabilities.diagnostic == RemoteCapabilityState::Available
        });
        let workspace_for_system = workspace_id.clone();
        let workspace_for_resource = workspace_id.clone();
        let workspace_for_process = workspace_id.clone();
        let workspace_for_network = workspace_id.clone();
        let workspace_for_disk = workspace_id.clone();
        let workspace_for_system_log = workspace_id.clone();
        let workspace_for_application_log = workspace_id.clone();
        let workspace_for_service = workspace_id.clone();
        let workspace_for_service_log = workspace_id.clone();
        let workspace_for_metadata = workspace_id.clone();
        let workspace_for_chunk = workspace_id.clone();
        let run_button = |id: &'static str,
                          label: &'static str,
                          operation: SshDiagnosticOperation,
                          workspace_id: SshProfileId,
                          cx: &mut Context<Self>| {
            ramag_ui::clickable_button(id)
                .small()
                .label(label)
                .disabled(!available || diagnostic_loading)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.run_diagnostic(workspace_id.clone(), operation.clone(), cx);
                }))
        };
        let buttons = h_flex()
            .w_full()
            .flex_wrap()
            .gap(px(8.0))
            .child(run_button(
                "diagnostic-system-overview",
                "系统概况",
                SshDiagnosticOperation::SystemOverview,
                workspace_for_system,
                cx,
            ))
            .child(run_button(
                "diagnostic-resource-snapshot",
                "资源快照",
                SshDiagnosticOperation::ResourceSnapshot,
                workspace_for_resource,
                cx,
            ))
            .child(run_button(
                "diagnostic-process-list",
                "进程列表",
                SshDiagnosticOperation::ProcessList,
                workspace_for_process,
                cx,
            ))
            .child(run_button(
                "diagnostic-network-snapshot",
                "网络快照",
                SshDiagnosticOperation::NetworkSnapshot,
                workspace_for_network,
                cx,
            ))
            .child(run_button(
                "diagnostic-disk-overview",
                "磁盘概况",
                SshDiagnosticOperation::DiskOverview,
                workspace_for_disk,
                cx,
            ))
            .child(
                ramag_ui::clickable_button("diagnostic-system-log")
                    .small()
                    .label("系统日志")
                    .disabled(!available || diagnostic_loading)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.run_system_log(
                            workspace_for_system_log.clone(),
                            SshLogSource::System,
                            cx,
                        );
                    })),
            )
            .child(
                ramag_ui::clickable_button("diagnostic-application-log")
                    .small()
                    .label("应用日志")
                    .disabled(!available || diagnostic_loading)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.run_system_log(
                            workspace_for_application_log.clone(),
                            SshLogSource::Application,
                            cx,
                        );
                    })),
            )
            .child(
                ramag_ui::clickable_button("diagnostic-service-status")
                    .small()
                    .label("服务状态")
                    .disabled(!available || diagnostic_loading)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.prompt_service_status(workspace_for_service.clone(), window, cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("diagnostic-service-log")
                    .small()
                    .label("服务日志")
                    .disabled(!available || diagnostic_loading)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.prompt_service_log(workspace_for_service_log.clone(), window, cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("diagnostic-file-metadata")
                    .small()
                    .label("文件元信息")
                    .disabled(!available || diagnostic_loading || selected_path.is_none())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.run_selected_file_diagnostic(
                            workspace_for_metadata.clone(),
                            false,
                            cx,
                        );
                    })),
            )
            .child(
                ramag_ui::clickable_button("diagnostic-file-chunk")
                    .small()
                    .label("文件片段")
                    .disabled(!available || diagnostic_loading || selected_path.is_none())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.run_selected_file_diagnostic(workspace_for_chunk.clone(), true, cx);
                    })),
            );
        let action = if diagnostic_loading {
            ramag_ui::clickable_button("diagnostic-cancel")
                .outline()
                .small()
                .label("取消")
                .on_click(cx.listener({
                    let workspace_id = workspace_id.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        this.cancel_diagnostic(workspace_id.clone(), cx);
                    }
                }))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let result_body = if let Some(result) = diagnostic_result {
            let status = format!(
                "{} · {} ms · 退出 {:?} · {}{}",
                result.operation,
                result.elapsed_millis,
                result.exit_code,
                if result.truncated {
                    "已截断"
                } else {
                    "完整"
                },
                match result.termination {
                    ramag_domain::entities::DiagnosticTermination::Completed => String::new(),
                    termination => format!(" · {:?}", termination),
                }
            );
            let output_for_copy = result.output.clone();
            let filtered_output = filter_diagnostic_output(&result.output, &diagnostic_query);
            let workspace_for_save = workspace_id.clone();
            v_flex()
                .flex_1()
                .min_h_0()
                .gap(px(6.0))
                .child(div().text_xs().text_color(muted).child(status))
                .child(
                    h_flex()
                        .gap(px(8.0))
                        .child(
                            ramag_ui::clickable_button("diagnostic-copy-result")
                                .ghost()
                                .xsmall()
                                .label("复制结果")
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        output_for_copy.clone(),
                                    ));
                                })),
                        )
                        .child(
                            ramag_ui::clickable_button("diagnostic-save-result")
                                .ghost()
                                .xsmall()
                                .label("保存到本地")
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.save_diagnostic_result(
                                        workspace_for_save.clone(),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(action),
                )
                .child(
                    ramag_ui::cleanable_input(
                        &self.diagnostic_search,
                        "diagnostic-result-search-clear",
                        false,
                        cx,
                    )
                    .small(),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p(px(10.0))
                        .bg(cx.theme().muted)
                        .font_family(cx.theme().mono_font_family.clone())
                        .whitespace_normal()
                        .text_xs()
                        .child(filtered_output),
                )
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(if diagnostic_loading {
                    "执行中…"
                } else {
                    "选择一个固定诊断操作"
                })
                .into_any_element()
        };
        let error = diagnostic_error.map(|error| {
            div()
                .text_xs()
                .text_color(danger)
                .child(error)
                .into_any_element()
        });
        v_flex()
            .id("ssh-safe-diagnostic-pane")
            .debug_selector(|| "ssh-safe-diagnostic-pane".into())
            .size_full()
            .child(production_badge)
            .child(
                v_flex()
                    .flex_none()
                    .gap(px(8.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(border)
                    .child(div().text_xs().text_color(muted).child(status))
                    .child(buttons)
                    .when_some(error, |panel, error| panel.child(error)),
            )
            .child(result_body)
            .into_any_element()
    }
}
