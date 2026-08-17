//! 全局设置中的数据库连接配置导入 / 导出交互。

use std::collections::HashSet;
use std::io::Read as _;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{Context, Window};
use gpui_component::notification::Notification;
use ramag_app::connection_transfer::{
    MAX_IMPORT_FILE_BYTES, MAX_TRANSFER_PASSPHRASE_BYTES, PreparedConnectionImport,
    decrypt_connection_import, encrypt_connection_export, prepare_connection_import,
    validate_connection_export_passphrase,
};
use ramag_domain::entities::{ConnectionConfig, ConnectionId};
use ramag_domain::error::DomainError;
use tracing::{error, info, warn};

use super::super::SettingsView;

impl SettingsView {
    pub(super) fn prompt_connection_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.database_transferring {
            return;
        }
        let entity = cx.entity().clone();
        crate::open_reveal_masked_prompt(
            "导出数据库连接",
            "导出 MySQL、PostgreSQL、Redis 和 MongoDB 连接。文件包含密码，请设置至少 8 个字符的口令。",
            "导出",
            |passphrase| validate_connection_export_passphrase(passphrase).err(),
            move |passphrase, _, app| {
                entity.update(app, |this, cx| this.export_connections(passphrase, cx));
            },
            window,
            cx,
        );
    }

    fn export_connections(&mut self, passphrase: String, cx: &mut Context<Self>) {
        if self.database_transferring {
            return;
        }
        self.database_transferring = true;
        cx.notify();
        let service = self.connection_service.clone();
        cx.spawn(async move |this, cx| {
            let outcome: Result<Option<String>, String> = async {
                let connections = service
                    .list()
                    .await
                    .map_err(|error| format!("读取连接列表失败：{error}"))?;
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| format!("读取系统时间失败：{error}"))?
                    .as_secs();
                let file_name = format!("ramag-connections-{timestamp}.json");
                let Some(handle) = rfd::AsyncFileDialog::new()
                    .set_file_name(&file_name)
                    .add_filter("Ramag JSON", &["json"])
                    .save_file()
                    .await
                else {
                    return Ok(None);
                };
                let path = handle.path().to_path_buf();
                let write_path = path.clone();
                ramag_app::run_blocking(move || {
                    let content = encrypt_connection_export(&connections, &passphrase)
                        .map_err(DomainError::Other)?;
                    ramag_app::usecases::export::write_atomic(&write_path, &content)
                })
                .await
                .map_err(|error| format!("写入导出文件失败：{error}"))?;
                Ok(Some(path.display().to_string()))
            }
            .await;

            let _ = this.update(cx, |this, cx| {
                this.database_transferring = false;
                match outcome {
                    Ok(None) => {}
                    Ok(Some(path)) => {
                        info!(
                            operation = "connection_export",
                            path = %path,
                            "database connection export completed"
                        );
                        this.pending_notification =
                            Some(Notification::success(format!("已加密导出到 {path}")));
                    }
                    Err(message) => {
                        error!(
                            operation = "connection_export",
                            error = %message,
                            "database connection export failed"
                        );
                        this.pending_notification = Some(Notification::error(message));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn import_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.database_transferring {
            return;
        }
        self.database_transferring = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let picked: Result<Option<PreparedConnectionImport>, String> = async {
                let Some(handle) = rfd::AsyncFileDialog::new()
                    .add_filter("Ramag JSON（兼容旧 .yaml）", &["json", "yaml", "yml"])
                    .pick_file()
                    .await
                else {
                    return Ok(None);
                };
                let path = handle.path().to_path_buf();
                let prepared = ramag_app::run_blocking(move || {
                    let file = std::fs::File::open(&path).map_err(|error| {
                        DomainError::Storage(format!("打开导入文件失败：{error}"))
                    })?;
                    let metadata = file.metadata().map_err(|error| {
                        DomainError::Storage(format!("读取文件信息失败：{error}"))
                    })?;
                    if !metadata.is_file() {
                        return Err(DomainError::InvalidConfig("导入目标必须是普通文件".into()));
                    }
                    if metadata.len() > MAX_IMPORT_FILE_BYTES {
                        return Err(DomainError::Storage(format!(
                            "文件过大：{} bytes，最多 {} bytes",
                            metadata.len(),
                            MAX_IMPORT_FILE_BYTES
                        )));
                    }
                    let mut raw = String::new();
                    file.take(MAX_IMPORT_FILE_BYTES + 1)
                        .read_to_string(&mut raw)
                        .map_err(|error| {
                            DomainError::Storage(format!("读取导入文件失败：{error}"))
                        })?;
                    if raw.len() as u64 > MAX_IMPORT_FILE_BYTES {
                        return Err(DomainError::Storage(format!(
                            "文件读取过程中超过 {MAX_IMPORT_FILE_BYTES} bytes 上限"
                        )));
                    }
                    prepare_connection_import(raw).map_err(DomainError::Other)
                })
                .await
                .map_err(|error| format!("读取导入文件失败：{error}"))?;
                Ok(Some(prepared))
            }
            .await;

            match picked {
                Ok(None) => {
                    let _ = this.update(cx, |this, cx| {
                        this.database_transferring = false;
                        cx.notify();
                    });
                }
                Err(message) => {
                    let _ = this.update(cx, |this, cx| {
                        this.database_transferring = false;
                        error!(operation = "connection_import", stage = "decrypt_or_parse", error = %message, "database connection import failed");
                        this.pending_notification = Some(Notification::error(message));
                        cx.notify();
                    });
                }
                Ok(Some(PreparedConnectionImport::Plain { valid, skipped })) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.confirm_plain_import(valid, skipped, window, cx);
                    });
                }
                Ok(Some(PreparedConnectionImport::Encrypted(raw))) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.database_transferring = false;
                        cx.notify();
                        let entity = cx.entity().clone();
                        crate::open_bounded_masked_prompt(
                            "输入导入口令",
                            "请输入导出时设置的口令。",
                            "",
                            "解密并导入",
                            MAX_TRANSFER_PASSPHRASE_BYTES,
                            move |passphrase, window, app| {
                                entity.update(app, |this, cx| {
                                    this.decrypt_and_import(raw, passphrase, window, cx);
                                });
                            },
                            window,
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
    }

    fn confirm_plain_import(
        &mut self,
        valid: Vec<ConnectionConfig>,
        skipped: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let service = self.connection_service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let existing = service.list().await;
            let _ = this.update_in(cx, move |this, window, cx| {
                this.database_transferring = false;
                match existing {
                    Ok(existing) => {
                        let (added, overwritten) = import_change_counts(&valid, &existing);
                        let entity = cx.entity().clone();
                        crate::open_confirm(
                            "导入未加密配置？",
                            format!(
                                "V1 明文文件可能含数据库密码。新增 {added} 个连接、覆盖 {overwritten} 个同 ID 连接，跳过 {} 个无效条目。请仅导入可信文件。",
                                skipped.len()
                            ),
                            "继续导入",
                            true,
                            move |_, app| {
                                entity.update(app, |this, cx| {
                                    this.save_imported(valid, skipped, cx);
                                });
                            },
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        error!(
                            operation = "connection_import",
                            stage = "existing_load",
                            error = %error,
                            "load existing database connections failed"
                        );
                        this.pending_notification = Some(Notification::error(format!(
                            "导入前读取现有连接失败：{error}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn decrypt_and_import(
        &mut self,
        raw: String,
        passphrase: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.database_transferring {
            return;
        }
        self.database_transferring = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = ramag_app::run_blocking(move || {
                decrypt_connection_import(&raw, &passphrase).map_err(DomainError::Other)
            })
            .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((valid, skipped)) => this.prepare_import_save(valid, skipped, window, cx),
                Err(error) => {
                    this.database_transferring = false;
                        error!(operation = "connection_import", stage = "decrypt", error = %error, "decrypt database connection import failed");
                    this.pending_notification =
                        Some(Notification::error(format!("解密失败：{error}")));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn prepare_import_save(
        &mut self,
        valid: Vec<ConnectionConfig>,
        skipped: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let service = self.connection_service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let existing = service.list().await;
            let _ = this.update_in(cx, move |this, window, cx| match existing {
                Ok(existing) => {
                    let (added, overwritten) = import_change_counts(&valid, &existing);
                    if overwritten == 0 {
                        this.save_imported(valid, skipped, cx);
                        return;
                    }
                    this.database_transferring = false;
                    let entity = cx.entity().clone();
                    crate::open_confirm(
                        "覆盖现有连接？",
                        format!(
                            "将新增 {added} 个连接、覆盖 {overwritten} 个同 ID 连接，跳过 {} 个无效条目。",
                            skipped.len()
                        ),
                        "继续导入",
                        true,
                        move |_, app| {
                            entity.update(app, |this, cx| {
                                this.save_imported(valid, skipped, cx);
                            });
                        },
                        window,
                        cx,
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.database_transferring = false;
                    error!(
                        operation = "connection_import",
                        stage = "existing_load",
                        error = %error,
                        "load existing database connections failed"
                    );
                    this.pending_notification = Some(Notification::error(format!(
                        "导入前读取现有连接失败：{error}"
                    )));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_imported(
        &mut self,
        valid: Vec<ConnectionConfig>,
        skipped: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.database_transferring = true;
        cx.notify();
        let service = self.connection_service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.save_many(&valid).await;
            let _ = this.update(cx, move |this, cx| {
                this.database_transferring = false;
                for entry in &skipped {
                    warn!(
                        operation = "connection_import",
                        entry = %entry,
                        "database connection import entry skipped"
                    );
                }
                match result {
                    Ok(()) => {
                        info!(
                            operation = "connection_import",
                            imported = valid.len(),
                            skipped = skipped.len(),
                            "database connections imported"
                        );
                        let message = format!(
                            "已导入 {} 个连接{}",
                            valid.len(),
                            if skipped.is_empty() {
                                String::new()
                            } else {
                                format!("，跳过 {} 个", skipped.len())
                            }
                        );
                        this.pending_notification = Some(if skipped.is_empty() {
                            Notification::success(message)
                        } else {
                            Notification::warning(message)
                        });
                    }
                    Err(error) => {
                        error!(
                            operation = "connection_import",
                            stage = "save",
                            error = %error,
                            "save imported database connections failed"
                        );
                        this.pending_notification = Some(Notification::error(format!(
                            "导入失败，未写入任何连接：{error}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn import_change_counts(
    incoming: &[ConnectionConfig],
    existing: &[ConnectionConfig],
) -> (usize, usize) {
    let existing_ids: HashSet<ConnectionId> =
        existing.iter().map(|config| config.id.clone()).collect();
    incoming
        .iter()
        .fold((0, 0), |(added, overwritten), config| {
            if existing_ids.contains(&config.id) {
                (added, overwritten + 1)
            } else {
                (added + 1, overwritten)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_counts_new_and_overwritten_connections() {
        let existing = ConnectionConfig::new_mysql("existing", "127.0.0.1", 3306, "root");
        let mut overwritten = existing.clone();
        overwritten.name = "updated".into();
        let added = ConnectionConfig::new_redis("new", "127.0.0.1", 6379);

        assert_eq!(
            import_change_counts(&[overwritten, added], &[existing]),
            (1, 1)
        );
    }
}
