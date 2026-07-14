//! 结果集导出 CSV/JSON/Markdown：序列化→rfd 保存→oneshot 回主线程→pending_notification→toast。
//! selected_rows 非空只导勾选行，否则全部

use std::path::PathBuf;

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
        let base = match &self.state {
            ResultState::Ok(r) => r,
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
        let (result, scope_label) = if !self.selected_rows.is_empty() {
            let mut filtered = base.as_ref().clone();
            let selected = self.selected_rows.clone();
            filtered.rows = base
                .rows
                .iter()
                .enumerate()
                .filter(|(i, _)| selected.contains(i))
                .map(|(_, r)| r.clone())
                .collect();
            if filtered.rows.is_empty() {
                self.pending_notification =
                    Some(Notification::warning("勾选的行越界，无内容可导出").autohide(true));
                cx.notify();
                return;
            }
            let n = filtered.rows.len();
            let visible = crate::views::result_table::compute_display_view(self, base, cx)
                .display_indices
                .into_iter()
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
            (filtered, scope)
        } else {
            let view = crate::views::result_table::compute_display_view(self, base, cx);
            if view.differs_from_raw(self) {
                // 视图与原始不同：按可见列投影 + 视图行序导出，与表格所见一致
                let cols = &view.visible_col_indices;
                let mut projected = base.as_ref().clone();
                projected.columns = cols.iter().map(|&i| base.columns[i].clone()).collect();
                projected.column_types = if base.column_types.is_empty() {
                    Vec::new()
                } else {
                    cols.iter()
                        .filter_map(|&i| base.column_types.get(i).cloned())
                        .collect()
                };
                projected.rows = view
                    .display_indices
                    .iter()
                    .filter_map(|&source_idx| base.rows.get(source_idx))
                    .map(|row| ramag_domain::entities::Row {
                        values: cols
                            .iter()
                            .map(|&i| {
                                row.values
                                    .get(i)
                                    .cloned()
                                    .unwrap_or(ramag_domain::entities::Value::Null)
                            })
                            .collect(),
                    })
                    .collect();
                if projected.rows.is_empty() {
                    self.pending_notification =
                        Some(Notification::warning("当前筛选下无行可导出").autohide(true));
                    cx.notify();
                    return;
                }
                let n = projected.rows.len();
                (projected, format!("当前视图（筛选/排序后）{n} 行"))
            } else {
                (
                    base.as_ref().clone(),
                    format!("全部 {} 行", base.rows.len()),
                )
            }
        };

        // 数据序列化（主线程毫秒级）
        let (content, default_name, ext) = match format {
            ExportFormat::Csv => (
                export::to_csv(&result),
                format!(
                    "ramag-export-{}.csv",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
                "csv",
            ),
            ExportFormat::Json => (
                export::to_json(&result),
                format!(
                    "ramag-export-{}.json",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
                "json",
            ),
            ExportFormat::Markdown => (
                export::to_markdown(&result),
                format!(
                    "ramag-export-{}.md",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
                "md",
            ),
        };

        // 异步：用 std::thread + futures::oneshot 把结果送回主线程（不依赖 tokio）
        let (tx, rx) = futures::channel::oneshot::channel::<ExportOutcome>();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .set_file_name(&default_name)
                .add_filter(ext, &[ext])
                .save_file();
            let outcome = match path {
                None => ExportOutcome::Cancelled,
                Some(p) => match std::fs::write(&p, content) {
                    Ok(_) => ExportOutcome::Saved(p),
                    Err(e) => ExportOutcome::Failed(e.to_string()),
                },
            };
            let _ = tx.send(outcome);
        });

        cx.spawn(async move |this, cx| {
            let outcome = rx.await.unwrap_or(ExportOutcome::Cancelled);
            let _ = this.update(cx, |this, cx| {
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
