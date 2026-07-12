//! 结果集导出：CSV（基于扁平表格）/ JSON（原始文档）。
//! rfd 保存框阻塞，放 std::thread 跑，结果经 oneshot 回主线程（与 dbclient 同款）。

use std::path::PathBuf;

use futures::channel::oneshot;
use gpui::Context;
use gpui_component::notification::Notification;

use super::ResultPanel;
use super::flatten::FlatTable;

impl ResultPanel {
    /// 导出当前结果，范围三档与表格所见一致：勾选行 >「当前视图（筛选/排序后）」> 全部。
    /// CSV 按可见列投影；JSON 按范围行导原始文档（文档本身不裁字段）
    pub(crate) fn export_documents(&mut self, as_csv: bool, cx: &mut Context<Self>) {
        let Some(result) = self.result.as_ref() else {
            return self.notify_error("无可导出的结果".to_string(), cx);
        };
        if result.documents.is_empty() {
            return self.notify_error("结果为空，无需导出".to_string(), cx);
        }
        let Some(table) = self.table.as_ref() else {
            return self.notify_error("无表格数据可导出".to_string(), cx);
        };

        // 行范围：勾选 > 行过滤视图 > 全部；再按当前排序列重排（与表格显示一致）
        let filtered = self.filtered_row_indices(cx);
        let (mut rows, scope) = if !self.selected_rows.is_empty() {
            let v: Vec<usize> = self
                .selected_rows
                .iter()
                .copied()
                .filter(|i| *i < table.rows.len())
                .collect();
            let n = v.len();
            (v, format!("选中 {n} 行"))
        } else if let Some(v) = filtered {
            let n = v.len();
            (v, format!("当前视图（筛选后）{n} 行"))
        } else {
            let n = table.rows.len();
            ((0..n).collect(), format!("全部 {n} 行"))
        };
        if rows.is_empty() {
            return self.notify_error("当前范围内无行可导出".to_string(), cx);
        }
        if let Some((sort_path, dir)) = self.sort_by.clone()
            && let Some(si) = table.columns.iter().position(|c| c.path == sort_path)
        {
            let numeric = matches!(
                table.columns[si].kind,
                "int" | "long" | "double" | "decimal"
            );
            rows.sort_by(|&a, &b| {
                let ord = super::table::compare_cells(
                    &table.rows[a][si].text,
                    &table.rows[b][si].text,
                    numeric,
                );
                if matches!(dir, super::SortDir::Desc) {
                    ord.reverse()
                } else {
                    ord
                }
            });
        }
        // 列范围：列过滤激活时仅导可见列（None = 全列）
        let cols: Vec<usize> = self
            .filtered_column_indices(cx)
            .unwrap_or_else(|| (0..table.columns.len()).collect());

        let (content, ext) = if as_csv {
            (flat_to_csv(table, &rows, &cols), "csv")
        } else {
            // JSON 按范围行导原始文档。注：钻取视图下表格行与原始文档非一一对应，
            // 此时可能取不到对应文档（导出为空则提示改用 CSV）
            let docs: Vec<&serde_json::Value> = rows
                .iter()
                .filter_map(|&i| result.documents.get(i))
                .collect();
            if docs.is_empty() {
                return self.notify_error(
                    "当前视图与原始文档不对应（钻取层），请改用 CSV 导出".to_string(),
                    cx,
                );
            }
            (
                serde_json::to_string_pretty(&docs).unwrap_or_default(),
                "json",
            )
        };
        let coll = self
            .target_collection
            .clone()
            .unwrap_or_else(|| "export".to_string());
        let name = format!("{coll}.{ext}");
        let scope_label = scope;
        // rfd 保存框是阻塞的：放 std::thread 跑，结果经 oneshot 回主线程（与 dbclient 同款）
        let (tx, rx) = oneshot::channel::<ExportOutcome>();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .set_file_name(&name)
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

/// FlatTable → CSV：按给定行序 / 可见列投影（与表格所见一致），逗号/引号/换行转义
fn flat_to_csv(table: &FlatTable, rows: &[usize], cols: &[usize]) -> String {
    let mut out = String::new();
    let header: Vec<String> = cols
        .iter()
        .filter_map(|&ci| table.columns.get(ci))
        .map(|c| csv_escape(&c.path))
        .collect();
    out.push_str(&header.join(","));
    out.push('\n');
    for &ri in rows {
        let Some(row) = table.rows.get(ri) else {
            continue;
        };
        let cells: Vec<String> = cols
            .iter()
            .map(|&ci| row.get(ci).map(|c| csv_escape(&c.text)).unwrap_or_default())
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

/// CSV 字段转义：含逗号 / 引号 / 换行时用双引号包裹，内部引号翻倍
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
