//! 结果集导出：CSV（基于扁平表格）/ JSON（原始文档）。
//! rfd 保存框异步等待，序列化放受限工作池，结果回主线程提示（与 dbclient 同款）。

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::usecases::export;
use ramag_domain::error::DomainError;
use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};

use super::ResultPanel;
use super::flatten::FlatTable;

impl ResultPanel {
    /// 导出当前结果，范围三档与表格所见一致：勾选行 >「当前视图（筛选/排序后）」> 全部。
    /// CSV 按可见列投影；JSON 按范围行导原始文档（文档本身不裁字段）
    pub(crate) fn export_documents(&mut self, as_csv: bool, cx: &mut Context<Self>) {
        if self.exporting {
            self.pending_notification = Some(
                Notification::info("已有导出任务正在进行")
                    .title("导出")
                    .autohide(true),
            );
            cx.notify();
            return;
        }
        if self.row_view_building {
            return self.notify_error("正在筛选 / 排序，请完成后再导出".to_string(), cx);
        }
        if let Some(error) = &self.row_view_error {
            return self.notify_error(format!("当前行视图不可用：{error}"), cx);
        }
        let Some(documents) = self.docs_arc.clone() else {
            return self.notify_error("无可导出的结果".to_string(), cx);
        };
        if documents.is_empty() {
            return self.notify_error("结果为空，无需导出".to_string(), cx);
        }
        let Some(table) = self.table.clone() else {
            return self.notify_error("无表格数据可导出".to_string(), cx);
        };

        // 行范围：勾选 > 行过滤视图 > 全部；再按当前排序列重排（与表格显示一致）
        let Some((display_rows, rows_filtered)) = self.display_row_indices(cx) else {
            return self.notify_error("当前行视图尚未准备完成".to_string(), cx);
        };
        let selected_scope = !self.selected_rows.is_empty();
        let (rows, scope) = if !self.selected_rows.is_empty() {
            let v: Vec<usize> = self
                .selected_rows
                .iter()
                .copied()
                .filter(|i| *i < table.rows.len())
                .collect();
            let n = v.len();
            let hidden = if rows_filtered {
                let visible: HashSet<usize> = display_rows.iter().copied().collect();
                self.selected_rows
                    .iter()
                    .filter(|ri| !visible.contains(ri))
                    .count()
            } else {
                0
            };
            let scope = if hidden > 0 {
                format!("选中 {n} 行，其中 {hidden} 行当前隐藏")
            } else {
                format!("选中 {n} 行")
            };
            (v, scope)
        } else {
            let n = display_rows.len();
            let scope = if rows_filtered {
                format!("当前视图（筛选后）{n} 行")
            } else {
                format!("全部 {n} 行")
            };
            (display_rows.as_ref().clone(), scope)
        };
        if rows.is_empty() {
            return self.notify_error("当前范围内无行可导出".to_string(), cx);
        }
        let selected_sort = if selected_scope
            && let Some((sort_path, dir)) = self.sort_by.clone()
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
        // 列范围：列过滤激活时仅导可见列（None = 全列）
        let cols: Vec<usize> = self
            .filtered_column_indices(cx)
            .unwrap_or_else(|| (0..table.columns.len()).collect());

        // JSON 按范围行导原始文档。钻取视图下表格行与原始文档可能不一一对应。
        if !as_csv && !rows.iter().any(|&index| documents.get(index).is_some()) {
            return self.notify_error(
                "当前视图与原始文档不对应（钻取层），请改用 CSV 导出".to_string(),
                cx,
            );
        }
        let ext = if as_csv { "csv" } else { "json" };
        let coll = self
            .target_collection
            .clone()
            .unwrap_or_else(|| "export".to_string());
        let name = format!("{coll}.{ext}");
        let scope_label = scope;
        // 用户取消时不做排序 / 序列化；保存框不占共享 worker，防重入避免重复弹框。
        self.exporting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = rfd::AsyncFileDialog::new()
                .set_file_name(&name)
                .add_filter(ext, &[ext])
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
                            if as_csv {
                                write_flat_csv(writer, &table, &rows, &cols).map_err(|error| {
                                    DomainError::Storage(format!(
                                        "写入 MongoDB CSV 导出内容失败：{error}"
                                    ))
                                })
                            } else {
                                serde_json::to_writer_pretty(
                                    writer,
                                    &SelectedDocuments {
                                        documents: documents.as_slice(),
                                        rows: &rows,
                                    },
                                )
                                .map_err(|error| {
                                    DomainError::Storage(format!(
                                        "写入 MongoDB JSON 导出内容失败：{error}"
                                    ))
                                })
                            }
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
                this.pending_notification = Some(match outcome {
                    ExportOutcome::Saved(p) => Notification::success(format!(
                        "{}（{}）",
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "导出完成".to_string()),
                        scope_label
                    ))
                    .title("导出成功")
                    .autohide(true),
                    ExportOutcome::Cancelled => Notification::info("已取消导出").autohide(true),
                    ExportOutcome::Failed(e) => {
                        Notification::error(e).title("导出失败").autohide(true)
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }
}

/// rfd 文件保存结果（线程 → 主线程）
enum ExportOutcome {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
}

/// FlatTable → CSV：按给定行序 / 可见列投影（与表格所见一致）。
fn write_flat_csv(
    writer: &mut dyn Write,
    table: &FlatTable,
    rows: &[usize],
    cols: &[usize],
) -> std::io::Result<()> {
    let mut first_header = true;
    for column in cols.iter().filter_map(|&index| table.columns.get(index)) {
        if !first_header {
            writer.write_all(b",")?;
        }
        write_csv_text(writer, &column.path)?;
        first_header = false;
    }
    writer.write_all(b"\n")?;
    for &ri in rows {
        let Some(row) = table.rows.get(ri) else {
            continue;
        };
        for (index, &column_index) in cols.iter().enumerate() {
            if index > 0 {
                writer.write_all(b",")?;
            }
            if let Some(cell) = row.get(column_index) {
                write_csv_text(writer, &cell.text)?;
            }
        }
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn write_csv_text(writer: &mut dyn Write, value: &str) -> std::io::Result<()> {
    if !value.contains([',', '"', '\n', '\r']) {
        return writer.write_all(value.as_bytes());
    }

    writer.write_all(b"\"")?;
    let bytes = value.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'"' {
            writer.write_all(&bytes[start..index])?;
            writer.write_all(b"\"\"")?;
            start = index + 1;
        }
    }
    writer.write_all(&bytes[start..])?;
    writer.write_all(b"\"")
}

struct SelectedDocuments<'a> {
    documents: &'a [serde_json::Value],
    rows: &'a [usize],
}

impl Serialize for SelectedDocuments<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let valid_count = self
            .rows
            .iter()
            .filter(|&&index| self.documents.get(index).is_some())
            .count();
        let mut sequence = serializer.serialize_seq(Some(valid_count))?;
        for &index in self.rows {
            if let Some(document) = self.documents.get(index) {
                sequence.serialize_element(document)?;
            }
        }
        sequence.end()
    }
}

#[cfg(test)]
mod tests {
    use super::super::cell::Cell;
    use super::super::flatten::Column;
    use super::*;

    #[test]
    fn flat_csv_streams_projection_and_escapes_fields() {
        let table = FlatTable {
            columns: vec![
                Column {
                    path: "name".into(),
                    kind: "text",
                },
                Column {
                    path: "note".into(),
                    kind: "text",
                },
            ],
            total_columns: 2,
            rows: vec![vec![
                Cell {
                    text: "A, B".into(),
                    kind: "text",
                },
                Cell {
                    text: "say \"hi\"\r\n".into(),
                    kind: "text",
                },
            ]],
        };
        let mut output = Vec::new();

        write_flat_csv(&mut output, &table, &[0], &[1, 0]).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "note,name\n\"say \"\"hi\"\"\r\n\",\"A, B\"\n"
        );
    }

    #[test]
    fn selected_documents_skip_invalid_rows_and_keep_order() {
        let documents = vec![serde_json::json!({"id": 1}), serde_json::json!({"id": 2})];
        let rows = [1, 99, 0];

        let value = serde_json::to_value(SelectedDocuments {
            documents: &documents,
            rows: &rows,
        })
        .unwrap();

        assert_eq!(value, serde_json::json!([{"id": 2}, {"id": 1}]));
    }
}
