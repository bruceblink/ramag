//! 树节点破坏性操作：清空集合 / 删除集合（视图）/ 删除数据库。
//! 右键菜单 → open_confirm 二次确认 → run_command → 刷新 + toast

use gpui::{Context, Entity};
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use ramag_domain::entities::{MAX_MONGO_COLLECTION_NAME_BYTES, validate_mongo_collection_name};
use ramag_ui::{open_bounded_prompt, open_confirm};
use serde_json::json;

use super::CollectionTreePanel;

// ===== 右键菜单构造（row.rs 调用） =====

/// collection / view 行右键菜单：清空集合（仅集合）+ 删除
pub(super) fn collection_context_menu(
    menu: PopupMenu,
    entity: Entity<CollectionTreePanel>,
    db: String,
    coll: String,
    is_view: bool,
) -> PopupMenu {
    // 重命名仅集合支持（renameCollection 不适用于 view），目标存在则服务端报错不覆盖
    let menu = if is_view {
        menu
    } else {
        let (d, c, ent) = (db.clone(), coll.clone(), entity.clone());
        menu.item(
            ramag_ui::menu_item("重命名").on_click(move |_, window, app| {
                let (d, c, ent) = (d.clone(), c.clone(), ent.clone());
                open_bounded_prompt(
                    "重命名集合",
                    format!("输入 {d}.{c} 的新名称"),
                    &c.clone(),
                    "重命名",
                    MAX_MONGO_COLLECTION_NAME_BYTES,
                    move |new_name, _, app| {
                        ent.update(app, |this, cx| this.rename_collection(d, c, new_name, cx));
                    },
                    window,
                    app,
                );
            }),
        )
    };

    let menu = if is_view {
        menu
    } else {
        let (d, c, ent) = (db.clone(), coll.clone(), entity.clone());
        menu.item(
            ramag_ui::menu_item("清空集合").on_click(move |_, window, app| {
                let (d, c, ent) = (d.clone(), c.clone(), ent.clone());
                open_confirm(
                    "清空集合",
                    format!("将删除 {d}.{c} 的全部文档（集合与索引保留），此操作不可恢复。"),
                    "清空",
                    true,
                    move |_, app| {
                        ent.update(app, |this, cx| this.clear_collection(d, c, cx));
                    },
                    window,
                    app,
                );
            }),
        )
    };

    let (label, title, desc) = if is_view {
        (
            "删除视图",
            "删除视图",
            format!("将删除视图 {db}.{coll}（仅删除视图定义，不影响源集合数据）。"),
        )
    } else {
        (
            "删除集合",
            "删除集合",
            format!("将永久删除集合 {db}.{coll}（文档与索引一并删除），此操作不可恢复。"),
        )
    };
    menu.item(ramag_ui::menu_item(label).on_click(move |_, window, app| {
        let (d, c, ent) = (db.clone(), coll.clone(), entity.clone());
        open_confirm(
            title,
            desc.clone(),
            "删除",
            true,
            move |_, app| {
                ent.update(app, |this, cx| this.drop_collection(d, c, cx));
            },
            window,
            app,
        );
    }))
}

/// database 行右键菜单：删除数据库
pub(super) fn database_context_menu(
    menu: PopupMenu,
    entity: Entity<CollectionTreePanel>,
    db: String,
) -> PopupMenu {
    menu.item(
        ramag_ui::menu_item("删除数据库").on_click(move |_, window, app| {
            let (db, ent) = (db.clone(), entity.clone());
            open_confirm(
                "删除数据库",
                format!("将永久删除数据库 {db} 及其中全部集合与数据，此操作不可恢复。"),
                "删除",
                true,
                move |_, app| {
                    ent.update(app, |this, cx| this.drop_database(db, cx));
                },
                window,
                app,
            );
        }),
    )
}

// ===== 命令执行 =====

impl CollectionTreePanel {
    fn begin_tree_mutation(&mut self, cx: &mut Context<Self>) -> Option<ramag_ui::MutationToken> {
        let Some(token) = self.mutation_gate.begin() else {
            self.pending_notification =
                Some(Notification::warning("上一项集合操作尚未完成，请稍候").autohide(true));
            cx.notify();
            return None;
        };
        cx.notify();
        Some(token)
    }

    /// delete 全部文档（q:{} + limit:0），集合与索引保留
    pub(super) fn clear_collection(&mut self, db: String, coll: String, cx: &mut Context<Self>) {
        let Some(conf) = self.connection.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let cmd = json!({"delete": coll.clone(), "deletes": [{"q": {}, "limit": 0}]});
        cx.spawn(async move |this, cx| {
            let r = svc.run_command(&conf, &db, cmd).await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                let current_connection =
                    this.connection.as_ref().map(|current| &current.id) == Some(&conf.id);
                if !current_connection || !current_mutation {
                    this.pending_notification = Some(match &r {
                        Ok(reply) => {
                            let n = reply.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                            Notification::success(format!(
                                "已在发起时的连接「{}」清空 {db}.{coll}，删除 {n} 个文档；当前树状态已变化，未自动刷新",
                                conf.name
                            ))
                            .autohide(true)
                        }
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                connection = %conf.name,
                                db = %db,
                                coll = %coll,
                                "clear collection failed after connection changed"
                            );
                            Notification::error(error.write_hint(&format!(
                                "发起时的连接「{}」清空失败",
                                conf.name
                            )))
                            .autohide(true)
                        }
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(reply) => {
                        let n = reply.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                        this.pending_notification = Some(
                            Notification::success(format!("已清空集合 {db}.{coll}，删除 {n} 个文档"))
                                .autohide(true),
                        );
                    }
                    Err(e) => {
                        tracing::error!(error = %e, db = %db, coll = %coll, "clear collection failed");
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("清空失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// renameCollection 必须对 admin 库执行，源 / 目标都是 "db.collection" 全名
    pub(super) fn rename_collection(
        &mut self,
        db: String,
        old: String,
        new: String,
        cx: &mut Context<Self>,
    ) {
        if new == old {
            return;
        }
        if let Err(error) = validate_mongo_collection_name(&new) {
            self.pending_notification =
                Some(Notification::error(error.message().to_string()).autohide(true));
            cx.notify();
            return;
        }
        let Some(conf) = self.connection.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let cmd = json!({
            "renameCollection": format!("{db}.{old}"),
            "to": format!("{db}.{new}"),
        });
        cx.spawn(async move |this, cx| {
            let r = svc.run_command(&conf, "admin", cmd).await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                let current_connection =
                    this.connection.as_ref().map(|current| &current.id) == Some(&conf.id);
                if !current_connection || !current_mutation {
                    this.pending_notification = Some(match &r {
                        Ok(_) => Notification::success(format!(
                            "已在发起时的连接「{}」完成重命名 {db}.{old} → {db}.{new}；当前树状态已变化，未自动刷新",
                            conf.name
                        ))
                        .autohide(true),
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                connection = %conf.name,
                                db = %db,
                                coll = %old,
                                "rename collection failed after connection changed"
                            );
                            Notification::error(error.write_hint(&format!(
                                "发起时的连接「{}」重命名失败",
                                conf.name
                            )))
                            .autohide(true)
                        }
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(_) => {
                        clear_selected_collection(&mut this.selected, &db, &old);
                        this.pending_notification = Some(
                            Notification::success(format!("已重命名为 {db}.{new}"))
                                .autohide(true),
                        );
                        cx.emit(super::TreeEvent::CollectionRenamed {
                            database: db.clone(),
                            old: old.clone(),
                            new: new.clone(),
                        });
                        this.load_collections(db.clone(), cx);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, db = %db, coll = %old, "rename collection failed");
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("重命名失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn drop_collection(&mut self, db: String, coll: String, cx: &mut Context<Self>) {
        let Some(conf) = self.connection.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let cmd = json!({"drop": coll.clone()});
        cx.spawn(async move |this, cx| {
            let r = svc.run_command(&conf, &db, cmd).await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                let current_connection =
                    this.connection.as_ref().map(|current| &current.id) == Some(&conf.id);
                if !current_connection || !current_mutation {
                    this.pending_notification = Some(match &r {
                        Ok(_) => Notification::success(format!(
                            "已在发起时的连接「{}」删除 {db}.{coll}；当前树状态已变化，未自动刷新",
                            conf.name
                        ))
                        .autohide(true),
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                connection = %conf.name,
                                db = %db,
                                coll = %coll,
                                "drop collection failed after connection changed"
                            );
                            Notification::error(error.write_hint(&format!(
                                "发起时的连接「{}」删除集合失败",
                                conf.name
                            )))
                            .autohide(true)
                        }
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(_) => {
                        clear_selected_collection(&mut this.selected, &db, &coll);
                        this.pending_notification = Some(
                            Notification::success(format!("已删除 {db}.{coll}")).autohide(true),
                        );
                        cx.emit(super::TreeEvent::CollectionDropped {
                            database: db.clone(),
                            collection: coll.clone(),
                        });
                        this.load_collections(db.clone(), cx);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, db = %db, coll = %coll, "drop collection failed");
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("删除失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn drop_database(&mut self, db: String, cx: &mut Context<Self>) {
        let Some(conf) = self.connection.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let cmd = json!({"dropDatabase": 1});
        cx.spawn(async move |this, cx| {
            let r = svc.run_command(&conf, &db, cmd).await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                let current_connection =
                    this.connection.as_ref().map(|current| &current.id) == Some(&conf.id);
                if !current_connection || !current_mutation {
                    this.pending_notification = Some(match &r {
                        Ok(_) => Notification::success(format!(
                            "已在发起时的连接「{}」删除数据库 {db}；当前树状态已变化，未自动刷新",
                            conf.name
                        ))
                        .autohide(true),
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                connection = %conf.name,
                                db = %db,
                                "drop database failed after connection changed"
                            );
                            Notification::error(error.write_hint(&format!(
                                "发起时的连接「{}」删除数据库失败",
                                conf.name
                            )))
                            .autohide(true)
                        }
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(_) => {
                        this.remove_expanded_entry(&db);
                        this.open_databases.remove(&db);
                        this.invalidate_tree_rows();
                        if this.active_db.as_deref() == Some(db.as_str()) {
                            this.active_db = None;
                        }
                        if this.selected.as_ref().is_some_and(|(d, _)| d == &db) {
                            this.selected = None;
                        }
                        this.pending_notification = Some(
                            Notification::success(format!("已删除数据库 {db}")).autohide(true),
                        );
                        this.refresh_databases(cx);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, db = %db, "drop database failed");
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("删除失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn clear_selected_collection(selected: &mut Option<(String, String)>, db: &str, coll: &str) {
    if selected
        .as_ref()
        .is_some_and(|(selected_db, selected_coll)| selected_db == db && selected_coll == coll)
    {
        *selected = None;
    }
}

#[cfg(test)]
mod tests {
    use super::clear_selected_collection;

    #[test]
    fn successful_collection_ddl_preserves_unrelated_selection() {
        let mut selected = Some(("app".to_string(), "users".to_string()));

        clear_selected_collection(&mut selected, "app", "posts");
        assert_eq!(
            selected
                .as_ref()
                .map(|(db, collection)| (db.as_str(), collection.as_str())),
            Some(("app", "users"))
        );

        clear_selected_collection(&mut selected, "app", "users");
        assert!(selected.is_none());
    }
}
