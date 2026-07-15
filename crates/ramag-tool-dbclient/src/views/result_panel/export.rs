//! 结果集导出 CSV/JSON/Markdown：rfd 选路径→受限工作池序列化/写入→回主线程 toast。
//! selected_rows 非空只导勾选行，否则全部

use std::path::PathBuf;
use std::sync::Arc;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::usecases::export;
use tracing::{error, info};

use super::ResultPanel;
use super::ResultState;

#[derive(Clone, Copy)]
pub enum ExportFormat {
    Csv,
    Json,
    Markdown,
}

enum ExportOutcome {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
}

impl ResultPanel {
    /// 导出为 CSV / JSON / Markdown
    pub fn export(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
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

        // 导出范围三档：勾选行 > 当前视图（有排序/过滤时，所见即所导）> 原始全量
        let (row_indices, column_indices, scope_label) = if !self.selected_rows.is_empty() {
            let selected = Arc::new(
                self.selected_rows
                    .iter()
                    .copied()
                    .filter(|index| *index < base.rows.len())
                    .collect::<Vec<_>>(),
            );
            if selected.is_empty() {
                self.pending_notification =
                    Some(Notification::warning("勾选的行越界，无内容可导出").autohide(true));
                cx.notify();
                return;
            }
            let n = selected.len();
            let visible = crate::views::result_table::compute_display_view(self, &base, cx)
                .display_indices
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let hidden = self
                .selected_rows
                .iter()
                .filter(|ri| !visible.contains(ri))
                .count();
            let scope = if hidden > 0 {
                format!("选中 {n} 行，其中 {hidden} 行当前隐藏")
            } else {
                format!("选中 {n} 行")
            };
            (Some(selected), None, scope)
        } else {
            let view = crate::views::result_table::compute_display_view(self, &base, cx);
            if view.differs_from_raw(self) {
                // 视图与原始不同：仅传共享索引，后台按可见列和显示行序流式投影。
                if view.display_indices.is_empty() {
                    self.pending_notification =
                        Some(Notification::warning("当前筛选下无行可导出").autohide(true));
                    cx.notify();
                    return;
                }
                let n = view.display_indices.len();
                (
                    Some(view.display_indices),
                    Some(view.visible_col_indices),
                    format!("当前视图（筛选/排序后）{n} 行"),
                )
            } else {
                (None, None, format!("全部 {} 行", base.rows.len()))
            }
        };

        let (default_name, ext) = match format {
            ExportFormat::Csv => (
                format!(
                    "ramag-export-{}.csv",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
                "csv",
            ),
            ExportFormat::Json => (
                format!(
                    "ramag-export-{}.json",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
                "json",
            ),
            ExportFormat::Markdown => (
                format!(
                    "ramag-export-{}.md",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
                "md",
            ),
        };

        // 保存框与序列化放入共享有界 worker；防重入避免重复弹框和无限创建线程。
        self.exporting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = ramag_app::run_blocking(move || {
                let path = rfd::FileDialog::new()
                    .set_file_name(&default_name)
                    .add_filter(ext, &[ext])
                    .save_file();
                Ok(match path {
                    None => ExportOutcome::Cancelled,
                    Some(p) => {
                        let rows = row_indices.as_deref().map(Vec::as_slice);
                        let columns = column_indices.as_deref().map(Vec::as_slice);
                        let write_result = export::write_atomic_with(&p, |writer| match format {
                            ExportFormat::Csv => {
                                export::write_csv_view(writer, &base, rows, columns)
                            }
                            ExportFormat::Json => {
                                export::write_json_view(writer, &base, rows, columns)
                            }
                            ExportFormat::Markdown => {
                                export::write_markdown_view(writer, &base, rows, columns)
                            }
                        });
                        match write_result {
                            Ok(_) => ExportOutcome::Saved(p),
                            Err(e) => ExportOutcome::Failed(format!(
                                "写入导出文件 {} 失败：{e}",
                                p.display()
                            )),
                        }
                    }
                })
            })
            .await
            .unwrap_or_else(|error| {
                ExportOutcome::Failed(format!("导出后台任务无法执行：{error}"))
            });
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
                        let short = if msg.chars().count() > 80 {
                            let truncated: String = msg.chars().take(80).collect();
                            format!("{truncated}…")
                        } else {
                            msg
                        };
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
