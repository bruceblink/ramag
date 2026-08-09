//! 数据库客户端主视图的初始化与启动恢复。

use super::*;

impl DbClientView {
    pub fn new(
        service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        data_sync_service: Arc<DataSyncService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| {
            ConnectionListPanel::new(
                service.clone(),
                redis_service.clone(),
                mongo_service.clone(),
                window,
                cx,
            )
        });

        let subs = vec![cx.subscribe_in(&picker, window, Self::on_picker_event)];

        // 跨重启恢复：读上次打开的连接 id 列表与全部连接配置，按保存顺序匹配。
        if let Some(storage) = ramag_ui::theme::storage_from_cx(cx) {
            let svc = service.clone();
            cx.spawn(async move |this, cx| {
                let (pref, adjusted) = match storage.get_preference(OPEN_SESSIONS_PREF).await {
                    Ok(Some(json)) => match parse_open_sessions(&json) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            tracing::warn!(
                                operation = "dbclient_session_restore",
                                stage = "parse",
                                error,
                                "parse open sessions preference failed"
                            );
                            let _ = this.update(cx, |this, cx| {
                                this.pending_notification = Some(
                                    gpui_component::notification::Notification::warning(
                                        "已忽略损坏的连接标签恢复数据",
                                    ),
                                );
                                cx.notify();
                            });
                            return;
                        }
                    },
                    Ok(None) => return,
                    Err(error) => {
                        tracing::warn!(
                            operation = "dbclient_session_restore",
                            stage = "preference_load",
                            error = %error,
                            "load open sessions preference failed"
                        );
                        let _ = this.update(cx, |this, cx| {
                            this.pending_notification = Some(
                                gpui_component::notification::Notification::warning(format!(
                                    "无法恢复上次打开的连接标签：{error}"
                                )),
                            );
                            cx.notify();
                        });
                        return;
                    }
                };
                if pref.ids.is_empty() {
                    return;
                }
                let all = match svc.list().await {
                    Ok(all) => all,
                    Err(error) => {
                        tracing::warn!(
                            operation = "dbclient_session_restore",
                            stage = "connections_load",
                            error = %error,
                            "load connections for session restore failed"
                        );
                        let _ = this.update(cx, |this, cx| {
                            this.pending_notification = Some(
                                gpui_component::notification::Notification::warning(format!(
                                    "无法恢复连接标签：{error}"
                                )),
                            );
                            cx.notify();
                        });
                        return;
                    }
                };
                let configs: Vec<ConnectionConfig> = pref
                    .ids
                    .iter()
                    .filter_map(|id| all.iter().find(|c| &c.id == id).cloned())
                    .collect();
                if configs.is_empty() {
                    return;
                }
                let _ = this.update(cx, |this, cx| {
                    this.pending_restore = Some((configs, pref.active));
                    if adjusted {
                        this.pending_notification = Some(
                            gpui_component::notification::Notification::warning(format!(
                                "上次连接标签包含重复或超限项，仅恢复前 {MAX_CONNECTION_SESSIONS} 个有效标签"
                            ))
                            .autohide(true),
                        );
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        Self {
            service,
            redis_service,
            mongo_service,
            data_sync_service,
            result_memory: ramag_ui::ResultMemoryBudget::default(),
            sessions: Vec::new(),
            active_session: None,
            // 启动时显示连接管理（用户挑选打开哪个）
            center: CenterMode::ConnectionPicker,
            picker,
            sessions_scroll: ScrollHandle::new(),
            pending_notification: None,
            pending_restore: None,
            restore_allowed: true,
            form_subscription: None,
            _subscriptions: subs,
        }
    }
}
