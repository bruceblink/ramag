//! 结果集导出 JSONL：rfd 选路径→受限工作池序列化/写入→回主线程 toast。
//! selected_rows 非空只导勾选行，否则全部

use std::path::PathBuf;
use std::sync::Arc;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::usecases::export;
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
        let Some(view) = crate::views::result_table::cached_display_view(self, &base, cx) else {
            self.pending_notification =
                Some(Notification::info("结果仍在筛选或排序，请稍候再导出").autohide(true));
            cx.notify();
            return;
        };

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
            let visible = view
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
                    view.cols_filtered.then_some(view.visible_col_indices),
                    format!("当前视图（筛选/排序后）{n} 行"),
                )
            } else {
                (None, None, format!("全部 {} 行", base.rows.len()))
            }
        };

        let default_name = format!(
            "ramag-export-{}.jsonl",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
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
                        let rows = row_indices.as_deref().map(Vec::as_slice);
                        let columns = column_indices.as_deref().map(Vec::as_slice);
                        export::write_atomic_with(&write_path, |writer| {
                            export::write_jsonl_view(writer, &base, rows, columns)
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
