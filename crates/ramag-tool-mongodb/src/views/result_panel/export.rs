//! 结果集导出 JSONL：每行一个原始文档，与集合级导入配对（导出文件可直接导回）。
//! rfd 保存框异步等待，序列化放受限工作池，结果回主线程提示（与 dbclient 同款）。

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use gpui::{Context, Window};
use gpui_component::notification::Notification;
use ramag_app::usecases::export;
use ramag_domain::error::DomainError;

use super::{ResultEvent, ResultPanel};

impl ResultPanel {
    /// 结果工具条「导入」：对当前目标集合发起 JSONL 导入；
    /// 确认后上抛事件，由 session 路由到集合树执行（进度条显示在树侧）
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
            "导入 JSONL 到集合",
            format!(
                "选择冲突策略与 .jsonl 文件（可多选），每行一个文档（支持 Extended JSON），\
                 导入到 {db}.{collection}。「跳过」重复 _id 跳过，「覆盖」先清空集合文档\
                 （保留索引，不可恢复），「停止」遇重复即报错。"
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
    /// 导出当前结果为 JSONL（每行一个原始文档，不裁字段），
    /// 范围三档与表格所见一致：勾选行 >「当前视图（筛选/排序后）」> 全部
    pub(crate) fn export_documents(&mut self, cx: &mut Context<Self>) {
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
        // 按范围行导原始文档。钻取视图下表格行与原始文档可能不一一对应。
        if !rows.iter().any(|&index| documents.get(index).is_some()) {
            return self.notify_error(
                "当前视图与原始文档不对应（钻取层），请返回上层后导出".to_string(),
                cx,
            );
        }
        let coll = self
            .target_collection
            .clone()
            .unwrap_or_else(|| "export".to_string());
        let name = format!("{coll}.jsonl");
        let scope_label = scope;
        // 用户取消时不做排序 / 序列化；保存框不占共享 worker，防重入避免重复弹框。
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

/// 按给定行序流式写 JSONL：每行一个紧凑 JSON 文档，越界行索引跳过
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
