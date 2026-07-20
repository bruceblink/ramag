//! 集合树的按库导出 / 导入入口。编排在 `ramag_app::usecases::transfer::mongo`，
//! 这里负责文件选择、进度槽 / 取消位、完成通知与树刷新

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::MongoService;
use ramag_app::usecases::transfer;
use ramag_domain::entities::{ConflictPolicy, ConnectionConfig, TransferProgress, TransferSummary};
use ramag_domain::error::{READ_ONLY_MESSAGE, Result};

use super::CollectionTreePanel;

impl CollectionTreePanel {
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

    pub(super) fn export_database_to_file(&mut self, db: String, cx: &mut Context<Self>) {
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

    pub(super) fn import_database_from_file(
        &mut self,
        db: String,
        policy: ConflictPolicy,
        cx: &mut Context<Self>,
    ) {
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
            let outcome = run_import(svc, config, db, policy, cancel.clone(), slot).await;
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
    svc: Arc<MongoService>,
    config: ConnectionConfig,
    db: String,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let file_name = format!(
        "{db}-{}.jsonl",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
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
    let summary =
        transfer::export_mongo_database(&svc, &config, &db, &path, &cancel, &progress).await?;
    Ok(Some((summary, path.display().to_string())))
}

async fn run_import(
    svc: Arc<MongoService>,
    config: ConnectionConfig,
    db: String,
    policy: ConflictPolicy,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let Some(handle) = rfd::AsyncFileDialog::new()
        .add_filter("JSONL", &["jsonl", "json"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let path = handle.path().to_path_buf();
    let progress = ramag_ui::progress_sink(slot);
    let summary = transfer::import_mongo_database(
        &svc,
        &config,
        &path,
        Some(&db),
        policy,
        &cancel,
        &progress,
    )
    .await?;
    Ok(Some((summary, path.display().to_string())))
}
