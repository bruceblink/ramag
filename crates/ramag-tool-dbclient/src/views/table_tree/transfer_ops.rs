//! 表树的按库导出 / 导入入口。编排在 `ramag_app::usecases::transfer`，
//! 这里负责文件选择、进度槽 / 取消位、完成通知与树刷新

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::ConnectionService;
use ramag_app::usecases::transfer;
use ramag_domain::entities::{ConflictPolicy, ConnectionConfig, TransferProgress, TransferSummary};
use ramag_domain::error::{READ_ONLY_MESSAGE, Result};
use tracing::error;

use super::TableTreePanel;

impl TableTreePanel {
    /// 传输前置检查：并发互斥 + 连接存在
    fn transfer_ready(&mut self, cx: &mut Context<Self>) -> Option<ConnectionConfig> {
        if self.transfer.active() {
            self.pending_notification = Some(
                Notification::warning("已有导出 / 导入在进行中，请先完成或取消").autohide(true),
            );
            cx.notify();
            return None;
        }
        self.connection.clone()
    }

    pub(super) fn export_schema_to_file(&mut self, schema: String, cx: &mut Context<Self>) {
        let Some(config) = self.transfer_ready(cx) else {
            return;
        };
        let (cancel, slot) = self.transfer.begin();
        ramag_ui::spawn_transfer_ticker(cx, cancel.clone(), |this: &Self, token| {
            this.transfer.is_current(token)
        });
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome = run_export(svc, config, schema, cancel.clone(), slot).await;
            let _ = this.update(cx, |this, cx| {
                if !this.transfer.finish(&cancel) {
                    return;
                }
                this.pending_notification =
                    ramag_ui::transfer_notification("导出", "文件未生成", outcome);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn import_schema_from_files(
        &mut self,
        schema: String,
        policy: ConflictPolicy,
        files: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            return;
        }
        let Some(config) = self.transfer_ready(cx) else {
            return;
        };
        if config.production {
            self.pending_notification = Some(Notification::error(READ_ONLY_MESSAGE).autohide(true));
            cx.notify();
            return;
        }
        let (cancel, slot) = self.transfer.begin();
        ramag_ui::spawn_transfer_ticker(cx, cancel.clone(), |this: &Self, token| {
            this.transfer.is_current(token)
        });
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome =
                run_import(svc, config, schema, policy, files, cancel.clone(), slot).await;
            let _ = this.update(cx, |this, cx| {
                if !this.transfer.finish(&cancel) {
                    return;
                }
                let imported = matches!(&outcome, Ok(Some(_)));
                this.pending_notification =
                    ramag_ui::transfer_notification("导入", "已完成部分保留", outcome);
                if imported {
                    // 导入可能新建了库 / 表，刷新树
                    this.refresh(cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 表级 JSONL 导入：多文件循环，按 JSON 键名匹配列插入
    pub(super) fn import_table_from_files(
        &mut self,
        schema: String,
        table: String,
        policy: ConflictPolicy,
        files: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            return;
        }
        let Some(config) = self.transfer_ready(cx) else {
            return;
        };
        if config.production {
            self.pending_notification = Some(Notification::error(READ_ONLY_MESSAGE).autohide(true));
            cx.notify();
            return;
        }
        let (cancel, slot) = self.transfer.begin();
        ramag_ui::spawn_transfer_ticker(cx, cancel.clone(), |this: &Self, token| {
            this.transfer.is_current(token)
        });
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome = run_table_import(
                svc,
                config,
                (schema, table),
                policy,
                files,
                cancel.clone(),
                slot,
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                if !this.transfer.finish(&cancel) {
                    return;
                }
                let imported = matches!(&outcome, Ok(Some(_)));
                this.pending_notification =
                    ramag_ui::transfer_notification("导入", "已完成部分保留", outcome);
                if imported {
                    this.refresh(cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

async fn run_export(
    svc: Arc<ConnectionService>,
    config: ConnectionConfig,
    schema: String,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let file_name = format!(
        "{schema}-{}.sql",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(&file_name)
        .add_filter("SQL", &["sql"])
        .save_file()
        .await
    else {
        return Ok(None);
    };
    let path = handle.path().to_path_buf();
    let progress = ramag_ui::progress_sink(slot);
    let summary =
        transfer::export_sql_database(&svc, &config, &schema, &path, &cancel, &progress).await?;
    Ok(Some((summary, path.display().to_string())))
}

/// 逐文件导入并汇总；任一文件出错即停止（出错文件名记入日志便于定位）
async fn run_import(
    svc: Arc<ConnectionService>,
    config: ConnectionConfig,
    schema: String,
    policy: ConflictPolicy,
    files: Vec<PathBuf>,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let progress = ramag_ui::progress_sink(slot);
    let file_count = files.len();
    let mut total = TransferSummary::default();
    let mut single_target = String::new();
    for path in files {
        if cancel.load(Ordering::Relaxed) {
            total.cancelled = true;
            break;
        }
        single_target = path.display().to_string();
        let summary = match transfer::import_sql_database(
            &svc,
            &config,
            &path,
            policy,
            Some(&schema),
            &cancel,
            &progress,
        )
        .await
        {
            Ok(summary) => summary,
            Err(e) => {
                error!(file = %path.display(), "sql import failed");
                return Err(e);
            }
        };
        let cancelled = summary.cancelled;
        total.merge(summary);
        if cancelled {
            break;
        }
    }
    let target = if file_count == 1 {
        single_target
    } else {
        format!("{file_count} 个文件")
    };
    Ok(Some((total, target)))
}

/// 表级 JSONL：逐文件导入并汇总；任一文件出错即停止（出错文件名记入日志便于定位）
async fn run_table_import(
    svc: Arc<ConnectionService>,
    config: ConnectionConfig,
    target: (String, String),
    policy: ConflictPolicy,
    files: Vec<PathBuf>,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let (schema, table) = target;
    let progress = ramag_ui::progress_sink(slot);
    let file_count = files.len();
    let mut total = TransferSummary::default();
    let mut single_target = String::new();
    for path in files {
        if cancel.load(Ordering::Relaxed) {
            total.cancelled = true;
            break;
        }
        single_target = path.display().to_string();
        let summary = match transfer::import_jsonl_into_table(
            &svc,
            &config,
            (&schema, &table),
            &path,
            policy,
            &cancel,
            &progress,
        )
        .await
        {
            Ok(summary) => summary,
            Err(e) => {
                error!(file = %path.display(), "jsonl table import failed");
                return Err(e);
            }
        };
        let cancelled = summary.cancelled;
        total.merge(summary);
        if cancelled {
            break;
        }
    }
    // 多文件都对同一张表：objects 累加会虚高，归一为 1
    total.objects = total.objects.min(1);
    let target = if file_count == 1 {
        single_target
    } else {
        format!("{file_count} 个文件 → {schema}.{table}")
    };
    Ok(Some((total, target)))
}
