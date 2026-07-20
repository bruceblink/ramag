//! Key 树的按 DB 导出 / 导入入口。编排在 `ramag_app::usecases::transfer::redis`，
//! 这里负责文件选择、进度槽 / 取消位、完成通知与重扫

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_app::RedisService;
use ramag_app::usecases::transfer;
use ramag_domain::entities::{ConflictPolicy, ConnectionConfig, TransferProgress, TransferSummary};
use ramag_domain::error::{READ_ONLY_MESSAGE, Result};

use super::KeyTreePanel;

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

    pub(super) fn import_db_from_file(&mut self, policy: ConflictPolicy, cx: &mut Context<Self>) {
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
            let outcome = run_import(svc, config, db, policy, cancel.clone(), slot).await;
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
}

async fn run_export(
    svc: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<TransferProgress>>,
) -> Result<Option<(TransferSummary, String)>> {
    let file_name = format!(
        "redis-db{db}-{}.jsonl",
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
    let summary = transfer::export_redis_db(&svc, &config, db, &path, &cancel, &progress).await?;
    Ok(Some((summary, path.display().to_string())))
}

async fn run_import(
    svc: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
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
    let summary =
        transfer::import_redis_db(&svc, &config, Some(db), &path, policy, &cancel, &progress)
            .await?;
    Ok(Some((summary, path.display().to_string())))
}
