//! 结果集选中行导出 JSONL：rfd 选路径→受限工作池序列化/写入→回主线程 toast。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::usecases::export;
use ramag_domain::entities::DriverKind;
use tracing::{error, info};

use super::ResultPanel;
use super::ResultState;

enum ExportOutcome {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
}

impl ResultPanel {
    /// 导出为 JSONL（每行一个 JSON 对象）
    pub fn export(&mut self, cx: &mut Context<Self>) {
        if self.exporting {
            self.pending_notification =
                Some(Notification::info("已有导出任务正在进行").autohide(true));
            cx.notify();
            return;
        }
        let base = match &self.state {
            ResultState::Ok(r) => r.clone(),
            _ => {
                self.pending_notification =
                    Some(Notification::warning("无可导出的结果").autohide(true));
                cx.notify();
                return;
            }
        };
        if base.rows.is_empty() {
            self.pending_notification =
                Some(Notification::warning("结果为空，无需导出").autohide(true));
            cx.notify();
            return;
        }
        if self.selected_rows.is_empty() {
            self.pending_notification = Some(Notification::warning("未选择数据").autohide(true));
            cx.notify();
            return;
        }
        let row_indices = Arc::new(
            self.selected_rows
                .iter()
                .copied()
                .filter(|index| *index < base.rows.len())
                .collect::<Vec<_>>(),
        );
        if row_indices.is_empty() {
            self.pending_notification =
                Some(Notification::warning("未选择有效数据").autohide(true));
            cx.notify();
            return;
        }
        let scope_label = format!("选中 {} 行", row_indices.len());

        let database_type = match self.connection.as_ref().map(|config| config.driver) {
            Some(DriverKind::Mysql) => "mysql",
            Some(DriverKind::Postgres) => "postgresql",
            Some(DriverKind::Redis | DriverKind::Mongodb) | None => "sql",
        };
        let database = self
            .pinned_target
            .as_ref()
            .and_then(|(schema, _)| schema.as_deref())
            .or_else(|| {
                self.connection
                    .as_ref()
                    .and_then(|config| config.database.as_deref())
            })
            .unwrap_or("query");
        let object = self.pinned_target.as_ref().map(|(_, table)| table.as_str());
        let default_name =
            export::suggested_export_file_name(database_type, database, object, true, "jsonl");
        let ext = "jsonl";

        // 保存框异步等待，不占用共享 worker；选定路径后才把序列化交给有界工作池。
        self.exporting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = rfd::AsyncFileDialog::new()
                .set_file_name(&default_name)
                .add_filter(ext, &[ext])
                .save_file()
                .await
                .map(|handle| handle.path().to_path_buf());
            let outcome = match path {
                None => ExportOutcome::Cancelled,
                Some(path) => {
                    let write_path = path.clone();
                    match ramag_app::run_blocking(move || {
                        export::write_atomic_with(&write_path, |writer| {
                            export::write_jsonl_view(writer, &base, Some(&row_indices), None)
                        })
                    })
                    .await
                    {
                        Ok(()) => ExportOutcome::Saved(path),
                        Err(error) => ExportOutcome::Failed(format!(
                            "写入导出文件 {} 失败：{error}",
                            path.display()
                        )),
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.exporting = false;
                let n = match outcome {
                    ExportOutcome::Saved(p) => {
                        info!(path = %p.display(), scope = %scope_label, "exported");
                        let file_name = p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "导出完成".to_string());
                        Notification::success(file_name)
                            .title(format!("导出成功 · {scope_label}"))
                            .autohide(true)
                    }
                    ExportOutcome::Cancelled => Notification::info("已取消导出").autohide(true),
                    ExportOutcome::Failed(msg) => {
                        error!(error = %msg, "export failed");
                        let short = msg
                            .char_indices()
                            .nth(80)
                            .map_or_else(|| msg.clone(), |(end, _)| format!("{}…", &msg[..end]));
                        Notification::error(short).title("导出失败").autohide(true)
                    }
                };
                this.pending_notification = Some(n);
                cx.notify();
            });
        })
        .detach();
    }
}
