//! 查询标签的手动提交事务控制。

use gpui::Context;
use gpui_component::notification::Notification;

use super::{QueryTab, TransactionSession};

impl QueryTab {
    /// Marks the active transaction dirty after a successful SQL mutation.
    pub(super) fn mark_transaction_dirty(&mut self, cx: &mut Context<Self>) {
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.dirty = true;
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

    pub(super) fn transaction_label(&self) -> &'static str {
        if self.transaction_busy {
            "事务处理中"
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
                        this.transaction = Some(TransactionSession { id, dirty: false });
                        this.pending_notification = Some(
                            Notification::success(
                                "已开启手动提交事务；编辑、删除和新增将在提交前暂存",
                            )
                            .autohide(true),
                        );
                    }
                    Err(error) => {
                        this.pending_notification = Some(
                            Notification::error(error.write_hint("开启事务失败")).autohide(true),
                        );
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
                    Ok(()) if commit => Notification::success("事务已提交").autohide(true),
                    Ok(()) => Notification::success("事务已回滚").autohide(true),
                    Err(error) if commit => {
                        Notification::error(error.write_hint("提交事务失败，事务已结束"))
                            .autohide(false)
                    }
                    Err(error) => Notification::error(error.write_hint("回滚事务失败，事务已结束"))
                        .autohide(false),
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
