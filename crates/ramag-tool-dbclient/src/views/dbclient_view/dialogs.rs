//! DbClientView 的弹窗与异步处理：连接表单 / 删除确认 / 异步删除

use gpui::{AppContext as _, Context, Entity, ParentElement, Styled, Window, px};
use gpui_component::WindowExt as _;
use ramag_domain::entities::{ConnectionConfig, ConnectionId, DriverKind};
use tracing::error;

use crate::views::connection_form::{self, ConnectionFormPanel, FormEvent};
use crate::views::data_sync_dialog::DataSyncDialog;

use super::{DbClientView, evict_connection_resources};

impl DbClientView {
    pub(super) fn open_data_sync(
        &mut self,
        target: ConnectionConfig,
        connections: &[ConnectionConfig],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target.driver == DriverKind::Redis {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(
                    "Redis 是缓存数据库，不提供数据同步",
                )
                .autohide(true),
            );
            cx.notify();
            return;
        }
        if self.data_sync_service.gate().is_blocking() {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(
                    "已有数据同步任务正在进行或等待确认",
                )
                .autohide(true),
            );
            cx.notify();
            return;
        }
        let service = self.data_sync_service.clone();
        let connections = connections.to_vec();
        let panel = cx.new(|cx| DataSyncDialog::new(service, target, &connections, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("数据同步")
                .close_button(false)
                .w(px(860.0))
                .pt(px(18.0))
                .px(px(20.0))
                .pb(px(16.0))
                .content({
                    let panel = panel.clone();
                    move |content, _, _| content.child(panel.clone())
                })
        });
    }

    pub(super) fn open_form_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let svc = self.service.clone();
        let redis_svc = self.redis_service.clone();
        let mongo_svc = self.mongo_service.clone();
        let form =
            cx.new(|cx| ConnectionFormPanel::new_create(svc, redis_svc, mongo_svc, window, cx));
        self.subscribe_form_and_open_dialog(form, window, cx);
    }

    pub(super) fn open_form_edit(
        &mut self,
        conn: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let redis_svc = self.redis_service.clone();
        let mongo_svc = self.mongo_service.clone();
        let form =
            cx.new(|cx| ConnectionFormPanel::new_edit(svc, redis_svc, mongo_svc, conn, window, cx));
        self.subscribe_form_and_open_dialog(form, window, cx);
    }

    /// 订阅表单事件并通过 dialog 系统弹出
    fn subscribe_form_and_open_dialog(
        &mut self,
        form: Entity<ConnectionFormPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sub = cx.subscribe_in(&form, window, Self::on_form_event);
        self.form_subscription = Some(sub);

        // 取一次 dialog 标题（mode 在 form 创建时就定了；不显示 driver 名，由表单内选择行体现）
        let title = {
            let f = form.read(cx);
            connection_form::dialog_title(f.mode()).to_string()
        };
        let form_for_dialog = form.clone();
        let view_for_close = cx.entity().clone();

        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let view_for_close = view_for_close.clone();
            let title = title.clone();
            dialog
                .title(title)
                // 右上角 X 会绕过 on_cancel 直接关闭，无法做脏保护；隐藏之，
                // 关闭统一走表单「取消」按钮 / Esc / 点遮罩（均有未保存修改确认）
                .close_button(false)
                // Esc / 点遮罩关闭前的脏保护：有修改先弹确认，确认「放弃修改」才关
                .on_cancel(move |_, window, app| {
                    if form_for_cancel.read(app).is_saving() {
                        return false;
                    }
                    if !form_for_cancel.read(app).is_dirty(app) {
                        return true;
                    }
                    let form_inner = form_for_cancel.clone();
                    ramag_ui::open_confirm(
                        "放弃修改？",
                        "表单有未保存的修改，关闭将丢弃这些修改。",
                        "放弃修改",
                        true,
                        move |_, app| {
                            // 发 Cancelled 走统一关闭路径（on_form_event 里 close_dialog）
                            form_inner.update(app, |_f, cx| {
                                cx.emit(connection_form::FormEvent::Cancelled)
                            });
                        },
                        window,
                        app,
                    );
                    false
                })
                .on_close(move |_, _, app| {
                    view_for_close.update(app, |this, _| this.form_subscription = None);
                })
                .w(px(720.0))
                .pt(px(24.0))
                .px(px(24.0))
                .pb(px(14.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    fn on_form_event(
        &mut self,
        _form: &Entity<ConnectionFormPanel>,
        event: &FormEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form_subscription = None;
        match event {
            FormEvent::Saved(conn) => {
                window.close_dialog(cx);
                self.picker.update(cx, |p, cx| {
                    p.invalidate_version(&conn.id);
                    p.refresh(cx);
                });
                // 失效 driver 内的连接池缓存：池按 ConnectionId 索引，旧 config 建的池
                // 还指向旧 host/db，必须丢弃，下次访问按新 config 重建
                evict_connection_resources(
                    &self.service,
                    &self.redis_service,
                    &self.mongo_service,
                    &conn.id,
                );
                // 编辑场景：旧标签的实体仍持旧配置（host / 库 / production 只读态都可能已变），
                // 继续使用存在按旧配置读写的风险。立即丢弃旧实体（终止其后台元数据刷新与
                // 在途等待，与关标签同语义）并置 stale，中央区显示"重新连接"面板；
                // 手写草稿按连接 id 持久化，重连后自动恢复，不会丢失
                let any_stale = self.mark_sessions_stale(conn);
                if any_stale {
                    self.pending_notification = Some(
                        gpui_component::notification::Notification::info(
                            "连接配置已更新。已打开的标签已暂停使用旧配置，请在标签页中重新连接（草稿已保留）",
                        )
                        .autohide(true),
                    );
                }
                cx.notify();
            }
            FormEvent::Cancelled => {
                window.close_dialog(cx);
            }
        }
    }

    /// 保存连接后同步新配置到同 id 的打开槽位：有实体的丢弃实体并置 stale（暂停使用），
    /// 惰性占位只更新配置（下次激活自然按新配置连）。返回是否有槽位被置 stale
    pub(super) fn mark_sessions_stale(&mut self, conn: &ConnectionConfig) -> bool {
        let mut any_stale = false;
        for slot in self.sessions.iter_mut().filter(|s| s.config.id == conn.id) {
            slot.config = conn.clone();
            if slot.entity.take().is_some() || slot.stale {
                slot.stale = true;
                any_stale = true;
            }
        }
        any_stale
    }

    /// 弹出删除确认对话框；用户点「删除」后才真正执行 handle_delete
    pub(super) fn confirm_delete(
        &mut self,
        id: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 找到对应连接名（用于描述文案）；找不到就 fallback 到 id 后 6 位
        let conn_name = self
            .picker
            .read(cx)
            .connections()
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| {
                let s = id.to_string();
                s.chars().rev().take(6).collect::<String>()
            });

        let view = cx.entity();
        ramag_ui::open_confirm(
            "删除连接？",
            format!(
                "确定要删除连接「{conn_name}」吗？\n将同时清除该连接在本机的查询历史和未完成草稿，不影响数据库中的数据。此操作不可撤销。"
            ),
            "删除",
            true,
            move |_window, app| {
                view.update(app, |this, cx| {
                    this.handle_delete(id, cx);
                });
            },
            window,
            cx,
        );
    }

    fn handle_delete(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        let svc = self.service.clone();
        let redis_svc = self.redis_service.clone();
        let mongo_svc = self.mongo_service.clone();
        let id_for_async = id.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.delete(&id_for_async).await;
            if result.is_ok() {
                evict_connection_resources(&svc, &redis_svc, &mongo_svc, &id_for_async);
            }
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    error!(error = %e, "delete connection failed");
                    this.pending_notification = Some(
                        gpui_component::notification::Notification::error(format!(
                            "删除连接失败：{e}"
                        ))
                        .autohide(true),
                    );
                    cx.notify();
                    return;
                }
                let to_close: Vec<usize> = this
                    .sessions
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.config.id == id_for_async)
                    .map(|(i, _)| i)
                    .collect();
                for idx in to_close.into_iter().rev() {
                    this.close_session(idx, cx);
                }
                this.picker.update(cx, |p, cx| p.refresh(cx));
                cx.notify();
            });
        })
        .detach();
    }
}
