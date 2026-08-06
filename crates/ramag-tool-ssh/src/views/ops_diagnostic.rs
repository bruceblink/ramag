//! 生产低影响只读诊断交互。

use gpui::{Context, Window};
use ramag_domain::entities::{
    DiagnosticCancellation, DiagnosticTimeRange, RemoteFileChunkPosition, RemotePath,
    SshDiagnosticOperation, SshLogSource, SshProfileId, SshServiceName, contains_case_insensitive,
};

use super::SshView;
use super::model::Notice;

pub(super) fn filter_diagnostic_output(output: &str, query: &str) -> String {
    if query.trim().is_empty() {
        return output.to_string();
    }
    output
        .lines()
        .filter(|line| contains_case_insensitive(line, query.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

impl SshView {
    pub(super) fn run_diagnostic(
        &mut self,
        workspace_id: SshProfileId,
        operation: SshDiagnosticOperation,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace_mut(&workspace_id) else {
            return;
        };
        if !workspace.profile.production {
            self.notice = Some(Notice::error("安全诊断只允许生产模式连接使用"));
            cx.notify();
            return;
        }
        if workspace.diagnostic_loading {
            return;
        }
        if let Err(error) = operation.validate() {
            workspace.diagnostic_error = Some(error);
            cx.notify();
            return;
        }
        workspace.diagnostic_generation = workspace.diagnostic_generation.wrapping_add(1);
        let generation = workspace.diagnostic_generation;
        let cancellation = DiagnosticCancellation::default();
        workspace.diagnostic_loading = true;
        workspace.diagnostic_error = None;
        workspace.diagnostic_cancellation = Some(cancellation.clone());
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service
                .execute_diagnostic(&workspace_id, &operation, cancellation)
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(workspace) = this.workspace_mut(&workspace_id) else {
                    return;
                };
                if workspace.diagnostic_generation != generation {
                    return;
                }
                workspace.diagnostic_loading = false;
                workspace.diagnostic_cancellation = None;
                match result {
                    Ok(result) => {
                        workspace.diagnostic_result = Some(result);
                        workspace.diagnostic_error = None;
                    }
                    Err(error) => workspace.diagnostic_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn cancel_diagnostic(&mut self, workspace_id: SshProfileId, cx: &mut Context<Self>) {
        if let Some(cancellation) = self
            .workspace_mut(&workspace_id)
            .and_then(|workspace| workspace.diagnostic_cancellation.clone())
        {
            cancellation.cancel();
            cx.notify();
        }
    }

    pub(super) fn run_system_log(
        &mut self,
        workspace_id: SshProfileId,
        source: SshLogSource,
        cx: &mut Context<Self>,
    ) {
        let since = match DiagnosticTimeRange::last_minutes(60) {
            Ok(since) => since,
            Err(error) => {
                self.notice = Some(Notice::error(error));
                cx.notify();
                return;
            }
        };
        self.run_diagnostic(
            workspace_id,
            SshDiagnosticOperation::LogQuery {
                source,
                service: None,
                max_items: 500,
                since: Some(since),
            },
            cx,
        );
    }

    pub(super) fn prompt_service_status(
        &mut self,
        workspace_id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        ramag_ui::open_bounded_prompt(
            "服务状态",
            "精确服务名",
            "",
            "读取状态",
            128,
            move |name, _window, app| {
                entity.update(app, |this, cx| match SshServiceName::parse(name.trim()) {
                    Ok(name) => this.run_diagnostic(
                        workspace_id,
                        SshDiagnosticOperation::ServiceStatus { name },
                        cx,
                    ),
                    Err(error) => {
                        this.notice = Some(Notice::error(error));
                        cx.notify();
                    }
                });
            },
            window,
            cx,
        );
    }

    pub(super) fn prompt_service_log(
        &mut self,
        workspace_id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        ramag_ui::open_bounded_prompt(
            "服务日志",
            "精确服务名（最近 60 分钟，最多 500 条）",
            "",
            "读取日志",
            128,
            move |name, _window, app| {
                entity.update(app, |this, cx| {
                    let result = SshServiceName::parse(name.trim()).and_then(|service| {
                        DiagnosticTimeRange::last_minutes(60).map(|since| (service, since))
                    });
                    match result {
                        Ok((service, since)) => this.run_diagnostic(
                            workspace_id,
                            SshDiagnosticOperation::LogQuery {
                                source: SshLogSource::Service,
                                service: Some(service),
                                max_items: 500,
                                since: Some(since),
                            },
                            cx,
                        ),
                        Err(error) => {
                            this.notice = Some(Notice::error(error));
                            cx.notify();
                        }
                    }
                });
            },
            window,
            cx,
        );
    }

    pub(super) fn run_selected_file_diagnostic(
        &mut self,
        workspace_id: SshProfileId,
        read_chunk: bool,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &workspace_id)
            .and_then(|workspace| {
                let namespace = workspace.capabilities.as_ref()?.sftp_namespace;
                let selected = workspace.selected_path.as_deref()?;
                RemotePath::parse_with_namespace(selected, namespace).ok()
            });
        let Some(path) = path else {
            self.notice = Some(Notice::error("请先选择当前 SFTP 命名空间中的文件"));
            cx.notify();
            return;
        };
        let operation = if read_chunk {
            SshDiagnosticOperation::FileChunk {
                path,
                position: RemoteFileChunkPosition::From(0),
            }
        } else {
            SshDiagnosticOperation::FileMetadata { path }
        };
        self.run_diagnostic(workspace_id, operation, cx);
    }

    pub(super) fn save_diagnostic_result(
        &mut self,
        workspace_id: SshProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &workspace_id)
            .and_then(|workspace| workspace.diagnostic_result.clone())
        else {
            return;
        };
        let file_name = format!("ramag-{}.txt", result.operation);
        cx.spawn_in(window, async move |this, async_cx| {
            let picked = rfd::AsyncFileDialog::new()
                .set_file_name(&file_name)
                .add_filter("文本", &["txt"])
                .save_file()
                .await;
            let Some(handle) = picked else {
                return;
            };
            let path = handle.path().to_path_buf();
            let contents = result.output.into_bytes();
            let write_path = path.clone();
            let write = ramag_app::run_blocking(move || {
                std::fs::write(&write_path, contents).map_err(|error| {
                    ramag_domain::DomainError::Other(format!("写入本地诊断结果失败：{error}"))
                })
            })
            .await;
            let _ = this.update_in(async_cx, |this, _window, cx| {
                this.notice = Some(match write {
                    Ok(()) => Notice::info(format!("诊断结果已保存到 {}", path.display())),
                    Err(error) => Notice::error(format!("保存诊断结果失败：{error}")),
                });
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::filter_diagnostic_output;

    #[test]
    fn diagnostic_filter_keeps_matching_lines_without_expanding_output() {
        assert_eq!(
            filter_diagnostic_output("CPU 10%\nMemory 20%\n", "memory"),
            "Memory 20%"
        );
        assert_eq!(filter_diagnostic_output("a\nb\n", ""), "a\nb\n");
    }
}
