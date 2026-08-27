//! 查询标签的手动提交事务控制。

use gpui::Context;
use gpui_component::notification::Notification;

use super::{QueryTab, TransactionSavepoint, TransactionSession};

pub(super) const MAX_TRANSACTION_SAVEPOINTS: usize = 32;

impl QueryTab {
    /// Marks the active transaction dirty after a successful SQL mutation.
    pub(super) fn mark_transaction_dirty(&mut self, cx: &mut Context<Self>) {
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.dirty = true;
            cx.notify();
        }
    }

    /// Keeps the latest failed transaction operation visible until the user finishes it.
    pub(super) fn mark_transaction_error(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        if self.transaction.is_some() || self.transaction_busy {
            self.transaction_error = Some(message.into());
            cx.notify();
        }
    }

    pub(crate) fn has_open_transaction(&self) -> bool {
        self.transaction.is_some() || self.transaction_busy
    }

    pub(super) fn transaction_is_dirty(&self) -> bool {
        self.transaction
            .as_ref()
            .is_some_and(|transaction| transaction.dirty)
    }

    pub(super) fn transaction_savepoints(&self) -> &[TransactionSavepoint] {
        self.transaction
            .as_ref()
            .map_or(&[], |transaction| transaction.savepoints.as_slice())
    }

    pub(super) fn transaction_label(&self) -> &'static str {
        if self.transaction_busy {
            "事务处理中"
        } else if self.transaction_error.is_some() {
            "事务异常"
        } else if self.transaction.is_some() {
            "手动提交"
        } else {
            "自动提交"
        }
    }

    /// Starts a driver-owned transaction and routes later row mutations to it.
    pub(super) fn begin_transaction(&mut self, cx: &mut Context<Self>) {
        if self.transaction.is_some() || self.transaction_busy || self.running {
            return;
        }
        let Some(connection) = self.connection.clone() else {
            self.pending_notification = Some(Notification::warning("尚未选择连接").autohide(true));
            cx.notify();
            return;
        };
        let service = self.service.clone();
        self.transaction_busy = true;
        self.transaction_error = None;
        self.transaction_seq = self.transaction_seq.wrapping_add(1);
        let request_seq = self.transaction_seq;
        self.sync_transaction_to_result(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = service.begin_transaction(&connection).await;
            let opened_id = outcome.as_ref().ok().cloned();
            let mut adopted = false;
            let _ = this.update(cx, |this, cx| {
                if this.transaction_seq != request_seq
                    || this
                        .connection
                        .as_ref()
                        .is_none_or(|current| current.id != connection.id)
                {
                    return;
                }
                this.transaction_busy = false;
                match outcome {
                    Ok(id) => {
                        adopted = true;
                        this.transaction = Some(TransactionSession {
                            id,
                            dirty: false,
                            savepoints: Vec::new(),
                            next_savepoint: 1,
                        });
                        this.pending_notification = Some(
                            Notification::success(
                                "已开启手动提交事务；编辑、删除和新增将在提交前暂存",
                            )
                            .autohide(true),
                        );
                    }
                    Err(error) => {
                        let message = error.write_hint("开启事务失败");
                        this.transaction_error = Some(message.clone());
                        this.pending_notification =
                            Some(Notification::error(message).autohide(true));
                    }
                }
                this.sync_transaction_to_result(cx);
                cx.notify();
            });
            if let Some(transaction_id) = opened_id
                && !adopted
                && let Err(error) = service
                    .rollback_transaction(&connection, &transaction_id)
                    .await
            {
                tracing::warn!(
                    operation = "sql_transaction_rollback_stale_begin",
                    connection_id = %connection.id,
                    transaction_id = %transaction_id,
                    error = %error,
                    "rollback of stale transaction begin failed"
                );
            }
        })
        .detach();
    }

    /// Creates a bounded, application-generated savepoint in the active transaction.
    pub(super) fn create_savepoint(&mut self, cx: &mut Context<Self>) {
        if self.transaction_busy || self.running {
            return;
        }
        let Some(session) = self.transaction.clone() else {
            return;
        };
        if session.savepoints.len() >= MAX_TRANSACTION_SAVEPOINTS {
            self.pending_notification = Some(
                Notification::warning(format!(
                    "保存点数量已达到上限（{MAX_TRANSACTION_SAVEPOINTS}）"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }
        let Some(connection) = self.connection.clone() else {
            return;
        };
        let name = format!("ramag_sp_{}", session.next_savepoint);
        let service = self.service.clone();
        self.transaction_busy = true;
        self.transaction_error = None;
        self.transaction_seq = self.transaction_seq.wrapping_add(1);
        let request_seq = self.transaction_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = service
                .create_savepoint(&connection, &session.id, &name)
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.transaction_request_matches(request_seq, &connection, &session.id) {
                    return;
                }
                this.transaction_busy = false;
                match outcome {
                    Ok(()) => {
                        if let Some(current) = this.transaction.as_mut() {
                            current.savepoints.push(TransactionSavepoint {
                                name: name.clone(),
                                dirty: current.dirty,
                            });
                            current.next_savepoint = current.next_savepoint.saturating_add(1);
                        }
                        this.pending_notification = Some(
                            Notification::success(format!("已创建保存点 {name}")).autohide(true),
                        );
                    }
                    Err(error) => {
                        let message = error.write_hint("创建保存点失败");
                        this.transaction_error = Some(message.clone());
                        this.pending_notification =
                            Some(Notification::error(message).autohide(false));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Rolls back to the most recent savepoint and keeps that savepoint available.
    pub(super) fn rollback_to_latest_savepoint(&mut self, cx: &mut Context<Self>) {
        let Some(savepoint) = self.transaction_savepoints().last().cloned() else {
            return;
        };
        self.rollback_to_savepoint(savepoint, cx);
    }

    fn rollback_to_savepoint(&mut self, savepoint: TransactionSavepoint, cx: &mut Context<Self>) {
        if self.transaction_busy || self.running {
            return;
        }
        let Some(session) = self.transaction.clone() else {
            return;
        };
        let Some(connection) = self.connection.clone() else {
            return;
        };
        let Some(target_index) = session
            .savepoints
            .iter()
            .position(|item| item.name == savepoint.name)
        else {
            return;
        };
        let service = self.service.clone();
        self.transaction_busy = true;
        self.transaction_error = None;
        self.transaction_seq = self.transaction_seq.wrapping_add(1);
        let request_seq = self.transaction_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = service
                .rollback_to_savepoint(&connection, &session.id, &savepoint.name)
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.transaction_request_matches(request_seq, &connection, &session.id) {
                    return;
                }
                this.transaction_busy = false;
                match outcome {
                    Ok(()) => {
                        if let Some(current) = this.transaction.as_mut() {
                            current.savepoints.truncate(target_index + 1);
                            current.dirty = savepoint.dirty;
                        }
                        this.pending_notification = Some(
                            Notification::success(format!("已回滚到保存点 {}", savepoint.name))
                                .autohide(true),
                        );
                    }
                    Err(error) => {
                        let message = error.write_hint("回滚到保存点失败");
                        this.transaction_error = Some(message.clone());
                        this.pending_notification =
                            Some(Notification::error(message).autohide(false));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Releases the most recent savepoint and any newer nested savepoints.
    pub(super) fn release_latest_savepoint(&mut self, cx: &mut Context<Self>) {
        let Some(savepoint) = self.transaction_savepoints().last().cloned() else {
            return;
        };
        if self.transaction_busy || self.running {
            return;
        }
        let Some(session) = self.transaction.clone() else {
            return;
        };
        let Some(connection) = self.connection.clone() else {
            return;
        };
        let Some(target_index) = session
            .savepoints
            .iter()
            .position(|item| item.name == savepoint.name)
        else {
            return;
        };
        let service = self.service.clone();
        self.transaction_busy = true;
        self.transaction_error = None;
        self.transaction_seq = self.transaction_seq.wrapping_add(1);
        let request_seq = self.transaction_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = service
                .release_savepoint(&connection, &session.id, &savepoint.name)
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.transaction_request_matches(request_seq, &connection, &session.id) {
                    return;
                }
                this.transaction_busy = false;
                match outcome {
                    Ok(()) => {
                        if let Some(current) = this.transaction.as_mut() {
                            current.savepoints.truncate(target_index);
                        }
                        this.pending_notification = Some(
                            Notification::success(format!("已释放保存点 {}", savepoint.name))
                                .autohide(true),
                        );
                    }
                    Err(error) => {
                        let message = error.write_hint("释放保存点失败");
                        this.transaction_error = Some(message.clone());
                        this.pending_notification =
                            Some(Notification::error(message).autohide(false));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn transaction_request_matches(
        &self,
        request_seq: u64,
        connection: &ramag_domain::entities::ConnectionConfig,
        transaction_id: &ramag_domain::entities::TransactionId,
    ) -> bool {
        self.transaction_seq == request_seq
            && self
                .connection
                .as_ref()
                .is_some_and(|current| current.id == connection.id)
            && self
                .transaction
                .as_ref()
                .is_some_and(|current| current.id == *transaction_id)
    }

    /// Commits or rolls back the active transaction, then returns to auto-commit mode.
    pub(super) fn finish_transaction(&mut self, commit: bool, cx: &mut Context<Self>) {
        if self.transaction_busy || self.running {
            return;
        }
        let Some(session) = self.transaction.clone() else {
            return;
        };
        let Some(connection) = self.connection.clone() else {
            return;
        };
        let service = self.service.clone();
        self.transaction_busy = true;
        self.transaction_seq = self.transaction_seq.wrapping_add(1);
        let request_seq = self.transaction_seq;
        self.sync_transaction_to_result(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = if commit {
                service.commit_transaction(&connection, &session.id).await
            } else {
                service.rollback_transaction(&connection, &session.id).await
            };
            let _ = this.update(cx, |this, cx| {
                if this.transaction_seq != request_seq
                    || this
                        .connection
                        .as_ref()
                        .is_none_or(|current| current.id != connection.id)
                {
                    return;
                }
                this.transaction_busy = false;
                this.transaction = None;
                this.sync_transaction_to_result(cx);
                this.pending_notification = Some(match outcome {
                    Ok(()) if commit => {
                        this.transaction_error = None;
                        Notification::success("事务已提交").autohide(true)
                    }
                    Ok(()) => {
                        this.transaction_error = None;
                        Notification::success("事务已回滚").autohide(true)
                    }
                    Err(error) if commit => {
                        let message = error.write_hint("提交事务失败，事务已结束");
                        this.transaction_error = Some(message.clone());
                        Notification::error(message).autohide(false)
                    }
                    Err(error) => {
                        let message = error.write_hint("回滚事务失败，事务已结束");
                        this.transaction_error = Some(message.clone());
                        Notification::error(message).autohide(false)
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Rolls back without waiting when a query tab is closed or its context changes.
    pub(crate) fn rollback_transaction_detached(&mut self, cx: &mut Context<Self>) {
        let was_busy = self.transaction_busy;
        let session = self.transaction.take();
        self.transaction_busy = false;
        self.transaction_error = None;
        self.transaction_seq = self.transaction_seq.wrapping_add(1);
        self.sync_transaction_to_result(cx);
        // A commit/rollback request already owns the driver slot; invalidating the
        // UI response is enough and avoids racing a second finish request.
        if was_busy {
            cx.notify();
            return;
        }
        let (Some(session), Some(connection)) = (session, self.connection.clone()) else {
            cx.notify();
            return;
        };
        let service = self.service.clone();
        cx.spawn(async move |_this, _cx| {
            if let Err(error) = service.rollback_transaction(&connection, &session.id).await {
                tracing::warn!(
                    operation = "sql_transaction_rollback_on_context_change",
                    connection_id = %connection.id,
                    transaction_id = %session.id,
                    error = %error,
                    "rollback after query context change failed"
                );
            }
        })
        .detach();
    }

    fn sync_transaction_to_result(&self, cx: &mut Context<Self>) {
        let id = self.transaction.as_ref().map(|session| session.id.clone());
        let busy = self.transaction_busy;
        self.result.update(cx, |result, cx| {
            result.set_transaction(id, busy, cx);
        });
    }
}
