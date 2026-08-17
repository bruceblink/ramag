//! 结果 JSONL 导入与导出。

use std::io::Write;
use std::path::PathBuf;

use gpui::{Context, Window};
use gpui_component::notification::Notification;
use ramag_app::usecases::export;
use ramag_domain::error::DomainError;
use tracing::{error, info};

use super::{ResultEvent, ResultPanel};

impl ResultPanel {
    /// 打开集合 JSONL 导入对话框。
    pub(crate) fn open_import_jsonl_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_write() {
            return;
        }
        let Some(collection) = self.target_collection.clone() else {
            return;
        };
        let db = self.database.clone();
        let entity = cx.entity();
        ramag_ui::open_import_options_dialog(
            "导入集合 JSONL",
            format!(
                "选择 JSONL 导入 {db}.{collection}。每行一个 Extended JSON 文档；\
                 跳过重复 _id，覆盖清空文档（保留索引）。"
            ),
            false,
            ("JSONL", &["jsonl", "json"]),
            move |policy, files, _, app| {
                entity.update(app, |_, cx| {
                    cx.emit(ResultEvent::CollectionImportRequested {
                        db,
                        collection,
                        policy,
                        files,
                    });
                });
            },
            window,
            cx,
        );
    }
    pub(crate) fn export_documents(&mut self, cx: &mut Context<Self>) {
        if self.exporting {
            self.pending_notification = Some(Notification::info("导出进行中").autohide(true));
            cx.notify();
            return;
        }
        let Some(documents) = self.docs_arc.clone() else {
            return self.notify_error("没有可导出的结果".to_string(), cx);
        };
        if documents.is_empty() {
            return self.notify_error("结果为空".to_string(), cx);
        }
        if self.selected_rows.is_empty() {
            self.pending_notification = Some(Notification::warning("请选择数据").autohide(true));
            cx.notify();
            return;
        }
        if self.parse_column_filter(cx).drill_path.is_some() {
            return self.notify_error("请先退出钻取再导出".to_string(), cx);
        }
        let Some(table) = self.table.clone() else {
            return self.notify_error("没有可导出的表格".to_string(), cx);
        };

        let rows: Vec<usize> = self
            .selected_rows
            .iter()
            .copied()
            .filter(|i| *i < table.rows.len())
            .collect();
        if rows.is_empty() {
            return self.notify_error("没有有效的选中数据".to_string(), cx);
        }
        let scope = format!("选中 {} 行", rows.len());
        let selected_sort = if let Some((sort_path, dir)) = self.sort_by.clone()
            && let Some(si) = table.columns.iter().position(|c| c.path == sort_path)
        {
            let numeric = matches!(
                table.columns[si].kind,
                "int" | "long" | "double" | "decimal"
            );
            Some((si, numeric, dir))
        } else {
            None
        };
        // 确认所选行对应当前文档。
        if !rows.iter().any(|&index| documents.get(index).is_some()) {
            return self.notify_error(
                "当前视图与原始文档不对应（钻取层），请返回上层后导出".to_string(),
                cx,
            );
        }
        let name = export::suggested_export_file_name(
            "mongodb",
            &self.database,
            self.target_collection.as_deref(),
            true,
            "jsonl",
        );
        let connection_id = self
            .config
            .as_ref()
            .map(|config| config.id.to_string())
            .unwrap_or_else(|| "-".to_string());
        let database = self.database.clone();
        let collection = self
            .target_collection
            .clone()
            .unwrap_or_else(|| "-".to_string());
        let scope_label = scope;
        // 选定文件后再占用工作池。
        self.exporting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = rfd::AsyncFileDialog::new()
                .set_file_name(&name)
                .add_filter("JSONL", &["jsonl"])
                .save_file()
                .await
                .map(|handle| handle.path().to_path_buf());
            let outcome = match path {
                None => ExportOutcome::Cancelled,
                Some(path) => {
                    let write_path = path.clone();
                    match ramag_app::run_blocking(move || {
                        let mut rows = rows;
                        if let Some((column_index, numeric, direction)) = selected_sort {
                            super::table::sort_row_indices(
                                &table,
                                column_index,
                                numeric,
                                direction,
                                &mut rows,
                            );
                        }
                        export::write_atomic_with(&write_path, |writer| {
                            write_selected_jsonl(writer, documents.as_slice(), &rows).map_err(
                                |error| {
                                    DomainError::Storage(format!(
                                        "写入 MongoDB JSONL 导出内容失败：{error}"
                                    ))
                                },
                            )
                        })
                    })
                    .await
                    {
                        Ok(()) => ExportOutcome::Saved(path),
                        Err(error) => ExportOutcome::Failed {
                            path,
                            error: error.to_string(),
                        },
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.exporting = false;
                this.pending_notification = match outcome {
                    ExportOutcome::Saved(path) => {
                        info!(
                            operation = "mongo_result_export",
                            connection_id = %connection_id,
                            database = %database,
                            collection = %collection,
                            path = %path.display(),
                            scope = %scope_label,
                            "result export completed"
                        );
                        let file_name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        Some(
                            Notification::success(format!("已导出 {file_name}（{scope_label}）"))
                                .autohide(true),
                        )
                    }
                    ExportOutcome::Cancelled => None,
                    ExportOutcome::Failed { path, error } => {
                        error!(
                            operation = "mongo_result_export",
                            connection_id = %connection_id,
                            database = %database,
                            collection = %collection,
                            error = %error,
                            path = %path.display(),
                            "result export failed"
                        );
                        Some(
                            Notification::error(format!(
                                "写入导出文件 {} 失败：{error}",
                                path.display()
                            ))
                            .autohide(true),
                        )
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }
}

enum ExportOutcome {
    Saved(PathBuf),
    Cancelled,
    Failed { path: PathBuf, error: String },
}

fn write_selected_jsonl(
    writer: &mut dyn Write,
    documents: &[serde_json::Value],
    rows: &[usize],
) -> std::io::Result<()> {
    for &index in rows {
        let Some(document) = documents.get(index) else {
            continue;
        };
        serde_json::to_writer(&mut *writer, document)?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_jsonl_skips_invalid_rows_and_keeps_order() {
        let documents = vec![serde_json::json!({"id": 1}), serde_json::json!({"id": 2})];
        let rows = [1, 99, 0];
        let mut output = Vec::new();

        write_selected_jsonl(&mut output, &documents, &rows).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"id\":2}\n{\"id\":1}\n"
        );
    }
}
