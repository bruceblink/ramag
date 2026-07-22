//! Key 树的 DB / 单 Key / 前缀导出与 DB 导入入口。
//! 这里负责文件选择、进度槽 / 取消位、完成通知与重扫

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::RedisService;
use ramag_app::usecases::{export, transfer};
use ramag_domain::entities::{ConflictPolicy, ConnectionConfig, TransferProgress, TransferSummary};
use ramag_domain::error::{READ_ONLY_MESSAGE, Result};
use tracing::error;

use super::KeyTreePanel;

enum ExportSelection {
    Key(String),
    Prefix(String),
}

impl KeyTreePanel {
    fn transfer_ready(&mut self, cx: &mut Context<Self>) -> Option<ConnectionConfig> {
        if self.transfer.active() {
            self.pending_notification = Some(
                Notification::warning("已有导出 / 导入在进行中，请先完成或取消").autohide(true),
            );
            cx.notify();
            return None;
        }
        self.config.clone()
    }

    pub(super) fn export_db_to_file(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.transfer_ready(cx) else {
            return;
        };
        let db = self.db;
        let (cancel, slot) = self.transfer.begin();
        ramag_ui::spawn_transfer_ticker(cx, cancel.clone(), |this: &Self, token| {
            this.transfer.is_current(token)
        });
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome = run_export(svc, config, db, cancel.clone(), slot).await;
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

    pub(super) fn export_key_to_file(&mut self, key: String, cx: &mut Context<Self>) {
        self.export_selection_to_file(ExportSelection::Key(key), cx);
    }

    pub(super) fn export_prefix_to_file(&mut self, prefix: String, cx: &mut Context<Self>) {
        self.export_selection_to_file(ExportSelection::Prefix(prefix), cx);
    }

    fn export_selection_to_file(&mut self, selection: ExportSelection, cx: &mut Context<Self>) {
        let Some(config) = self.transfer_ready(cx) else {
            return;
        };
        let db = self.db;
        let (cancel, slot) = self.transfer.begin();
        ramag_ui::spawn_transfer_ticker(cx, cancel.clone(), |this: &Self, token| {
            this.transfer.is_current(token)
        });
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome =
                run_selection_export(svc, config, db, selection, cancel.clone(), slot).await;
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

    pub(super) fn import_db_from_files(
        &mut self,
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
        let db = self.db;
        let (cancel, slot) = self.transfer.begin();
        ramag_ui::spawn_transfer_ticker(cx, cancel.clone(), |this: &Self, token| {
            this.transfer.is_current(token)
        });
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome = run_import(svc, config, db, policy, files, cancel.clone(), slot).await;
            let _ = this.update(cx, |this, cx| {
                if !this.transfer.finish(&cancel) {
                    return;
                }
                let imported = matches!(&outcome, Ok(Some(_)));
                this.pending_notification =
                    ramag_ui::transfer_notification("导入", "已完成部分保留", outcome);
                if imported {
                    // 树重扫，展示新导入的 key
                    this.refresh(cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// DB 级入口恢复单 Key / 前缀文件，文件范围由应用层校验。
    pub(super) fn import_selections_from_files(
        &mut self,
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
        let db = self.db;
        let (cancel, slot) = self.transfer.begin();
        ramag_ui::spawn_transfer_ticker(cx, cancel.clone(), |this: &Self, token| {
            this.transfer.is_current(token)
        });
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome =
                run_selection_import(svc, config, db, policy, files, cancel.clone(), slot).await;
            let _ = this.update(cx, |this, cx| {
                if !this.transfer.finish(&cancel) {
                    return;
                }
                let imported = matches!(&outcome, Ok(Some(_)));
                this.pending_notification =
                    ramag_ui::transfer_notification("导入 Key / 前缀", "已完成部分保留", outcome);
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
    svc: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let database = format!("db{db}");
    let file_name = export::suggested_export_file_name("redis", &database, None, false, "jsonl");
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(&file_name)
        .add_filter("JSONL", &["jsonl", "json"])
        .save_file()
        .await
    else {
        return Ok(None);
    };
    let path = handle.path().to_path_buf();
    let progress = ramag_ui::progress_sink(slot);
    let summary = transfer::export_redis_db(&svc, &config, db, &path, &cancel, &progress).await?;
    Ok(Some((summary, path.display().to_string())))
}

async fn run_selection_export(
    svc: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
    selection: ExportSelection,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let object = match &selection {
        ExportSelection::Key(key) => format!("key-{key}"),
        ExportSelection::Prefix(prefix) => format!("prefix-{prefix}"),
    };
    let database = format!("db{db}");
    let file_name =
        export::suggested_export_file_name("redis", &database, Some(&object), false, "jsonl");
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(&file_name)
        .add_filter("JSONL", &["jsonl", "json"])
        .save_file()
        .await
    else {
        return Ok(None);
    };
    let path = handle.path().to_path_buf();
    let progress = ramag_ui::progress_sink(slot);
    let summary = match selection {
        ExportSelection::Key(key) => {
            transfer::export_redis_key(&svc, &config, db, &key, &path, &cancel, &progress).await?
        }
        ExportSelection::Prefix(prefix) => {
            transfer::export_redis_prefix(&svc, &config, db, &prefix, &path, &cancel, &progress)
                .await?
        }
    };
    Ok(Some((summary, path.display().to_string())))
}

/// 逐文件导入并汇总；任一文件出错即停止（出错文件名记入日志便于定位）
async fn run_import(
    svc: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
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
        let summary = match transfer::import_redis_db(
            &svc,
            &config,
            Some(db),
            &path,
            policy,
            &cancel,
            &progress,
        )
        .await
        {
            Ok(summary) => summary,
            Err(e) => {
                error!(error = %e, file = %path.display(), scope = "database", "import failed");
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

async fn run_selection_import(
    svc: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
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
        let summary = match transfer::import_redis_selection(
            &svc, &config, db, &path, policy, &cancel, &progress,
        )
        .await
        {
            Ok(summary) => summary,
            Err(error) => {
                error!(error = %error, file = %path.display(), scope = "selection", "import failed");
                return Err(error);
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
        format!("{file_count} 个 Key / 前缀文件 → DB {db}")
    };
    Ok(Some((total, target)))
}
