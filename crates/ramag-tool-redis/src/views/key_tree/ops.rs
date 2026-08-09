//! 树节点破坏性操作：删除 key / 删除前缀下全部 key / 清空当前 DB。
//! 右键菜单（清空 DB 在工具栏「更多操作」，与普通节点操作隔离防误触）→
//! open_confirm 二次确认 → 异步执行 → 刷新 + toast；
//! 删除完成 emit KeysDeleted，上层据此清理详情面板

use gpui::{Context, Entity};
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use ramag_domain::entities::{MAX_REDIS_KEY_BYTES, RedisValue, validate_redis_key};
use ramag_ui::{open_bounded_prompt, open_confirm};

use super::helpers::{apply_local_rename, delete_by_pattern, escape_glob, truncate_label};
use super::{DeletedScope, KeyTreeEvent, KeyTreePanel};

/// key / 命名空间行右键菜单。两种身份兼具的节点（`user` 同时是 key 和前缀）两项都给
pub(super) fn node_context_menu(
    menu: PopupMenu,
    entity: Entity<KeyTreePanel>,
    full_path: String,
    is_leaf: bool,
    is_namespace: bool,
    allow_write: bool,
) -> PopupMenu {
    let mut menu = menu;
    if is_leaf {
        let (key, ent) = (full_path.clone(), entity.clone());
        menu = menu.item(ramag_ui::menu_item("导出").on_click(move |_, _, app| {
            ent.update(app, |this, cx| this.export_key_to_file(key.clone(), cx));
        }));
    }
    if is_namespace {
        let (prefix, ent) = (full_path.clone(), entity.clone());
        menu = menu.item(ramag_ui::menu_item("导出前缀").on_click(move |_, _, app| {
            ent.update(app, |this, cx| {
                this.export_prefix_to_file(prefix.clone(), cx)
            });
        }));
    }
    if !allow_write {
        return menu;
    }
    menu = menu.separator();
    if is_leaf {
        let (key, ent) = (full_path.clone(), entity.clone());
        menu = menu.item(ramag_ui::menu_item("改名").on_click(move |_, window, app| {
            let (key, ent) = (key.clone(), ent.clone());
            open_bounded_prompt(
                "重命名 Key",
                format!("输入「{}」的新名称", truncate_label(&key, 60)),
                &key.clone(),
                "改名",
                MAX_REDIS_KEY_BYTES,
                move |new_name, _, app| {
                    ent.update(app, |this, cx| this.rename_key_op(key, new_name, cx));
                },
                window,
                app,
            );
        }));
        let (key, ent) = (full_path.clone(), entity.clone());
        menu = menu.item(ramag_ui::menu_item("删除").on_click(move |_, window, app| {
            let (key, ent) = (key.clone(), ent.clone());
            open_confirm(
                "删除 Key",
                format!(
                    "将永久删除 key「{}」，此操作不可恢复。",
                    truncate_label(&key, 60)
                ),
                "删除",
                true,
                move |_, app| {
                    ent.update(app, |this, cx| this.delete_key_op(key, cx));
                },
                window,
                app,
            );
        }));
    }
    if is_namespace {
        let (prefix, ent) = (full_path.clone(), entity.clone());
        menu = menu.item(
            ramag_ui::menu_item("删除前缀").on_click(move |_, window, app| {
                let (prefix, ent) = (prefix.clone(), ent.clone());
                open_confirm(
                    "删除前缀下全部 Key",
                    format!(
                        "将删除匹配「{}:*」的全部 key（按服务端实际扫描，含未加载部分），此操作不可恢复。",
                        truncate_label(&prefix, 60)
                    ),
                    "删除",
                    true,
                    move |_, app| {
                        ent.update(app, |this, cx| this.delete_prefix_op(prefix, cx));
                    },
                    window,
                    app,
                );
            }),
        );
    }
    menu
}

/// 工具栏「更多」下拉菜单：新建 Key + DB 级毁灭性操作（清空 DB）。
/// 毁灭性操作与 key 节点右键菜单隔离，避免误触
pub(super) fn toolbar_more_menu(
    menu: PopupMenu,
    entity: Entity<KeyTreePanel>,
    db: u8,
) -> PopupMenu {
    let entity_for_create = entity.clone();
    let entity_for_export = entity.clone();
    let entity_for_import = entity.clone();
    let entity_for_selection_import = entity.clone();
    menu.item(
        ramag_ui::menu_item("新建").on_click(move |_, _window, app| {
            entity_for_create.update(app, |_this, cx| cx.emit(KeyTreeEvent::RequestCreate));
        }),
    )
    .item(
        ramag_ui::menu_item("导出库").on_click(move |_, _window, app| {
            entity_for_export.update(app, |this, cx| this.export_db_to_file(cx));
        }),
    )
    .item(
        ramag_ui::menu_item("导入库").on_click(move |_, window, app| {
            let ent = entity_for_import.clone();
            ramag_ui::open_import_options_dialog(
                "导入整个 Redis DB",
                format!(
                    "选择冲突策略与 .jsonl 文件（可多选），将导入到 DB {db}。重复导入同一文件：\
                     「跳过」按 key 断点续传，「覆盖」完全重建（幂等）。\
                     （list / string 无法条目级去重，Redis 不提供合并）"
                ),
                false,
                ("JSONL", &["jsonl", "json"]),
                move |policy, files, _, app| {
                    ent.update(app, |this, cx| this.import_db_from_files(policy, files, cx));
                },
                window,
                app,
            );
        }),
    )
    .item(
        ramag_ui::menu_item("导入对象").on_click(
            move |_, window, app| {
                let ent = entity_for_selection_import.clone();
                ramag_ui::open_import_options_dialog(
                    "导入对象",
                    format!(
                        "选择由 Ramag Key 或前缀节点“导出”生成的 .jsonl 文件（可多选），将 Key 类型、TTL 和全部值恢复到 DB {db}。Key 名取自文件。"
                    ),
                    false,
                    ("JSONL", &["jsonl", "json"]),
                    move |policy, files, _, app| {
                        ent.update(app, |this, cx| {
                            this.import_selections_from_files(policy, files, cx);
                        });
                    },
                    window,
                    app,
                );
            },
        ),
    )
    .separator()
    .item(
        ramag_ui::menu_item("清空库").on_click(move |_, window, app| {
            let ent = entity.clone();
            open_confirm(
                "清空库",
                format!("将删除 DB {db} 的全部 key（FLUSHDB），此操作不可恢复。"),
                "清空",
                true,
                move |_, app| {
                    ent.update(app, |this, cx| this.flush_db_op(cx));
                },
                window,
                app,
            );
        }),
    )
}

impl KeyTreePanel {
    fn begin_tree_mutation(&mut self, cx: &mut Context<Self>) -> Option<ramag_ui::MutationToken> {
        let Some(token) = self.mutation_gate.begin() else {
            self.pending_notification =
                Some(Notification::warning("上一项 Key 操作尚未完成，请稍候").autohide(true));
            cx.notify();
            return None;
        };
        cx.notify();
        Some(token)
    }

    /// RENAMENX：目标 key 已存在则返回 0 不覆盖，避免静默吞掉别人的数据
    pub(super) fn rename_key_op(&mut self, old: String, new: String, cx: &mut Context<Self>) {
        if new == old {
            return;
        }
        if let Err(error) = validate_redis_key(&new) {
            self.pending_notification =
                Some(Notification::error(error.message().to_string()).autohide(true));
            cx.notify();
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let argv = vec!["RENAMENX".to_string(), old.clone(), new.clone()];
            let r = svc.execute_command(&config, db, argv).await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                if !this.operation_context_matches(&config, db) || !current_mutation {
                    this.pending_notification = Some(match &r {
                        Ok(RedisValue::Int(1)) => Notification::success(format!(
                            "已在发起时的 DB {db} 完成重命名；当前树状态已变化，未自动刷新"
                        ))
                        .autohide(true),
                        Ok(RedisValue::Int(_)) => {
                            Notification::error("原 DB 中目标 key 已存在，未执行重命名")
                                .autohide(true)
                        }
                        Ok(_) => {
                            Notification::error("原 DB 重命名失败：服务端应答异常").autohide(true)
                        }
                        Err(error) => Notification::error(
                            error.write_hint(&format!("发起时的 DB {db} 重命名失败")),
                        )
                        .autohide(true),
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(RedisValue::Int(1)) => {
                        apply_local_rename(
                            &mut this.keys,
                            &mut this.seen_keys,
                            &mut this.key_bytes,
                            &old,
                            &new,
                        );
                        this.rebuild_tree();
                        if this.selected.as_deref() == Some(old.as_str()) {
                            this.selected = Some(new.clone());
                            cx.emit(KeyTreeEvent::Selected(new.clone()));
                        }
                        this.pending_notification = Some(
                            Notification::success(format!(
                                "已重命名为 {}",
                                truncate_label(&new, 60)
                            ))
                            .autohide(true),
                        );
                    }
                    Ok(RedisValue::Int(_)) => {
                        this.pending_notification = Some(
                            Notification::error("目标 key 已存在，未执行重命名").autohide(true),
                        );
                    }
                    Ok(_) => {
                        tracing::error!(
                            operation = "redis_key_rename",
                            connection_id = %config.id,
                            db,
                            old_key_bytes = old.len(),
                            new_key_bytes = new.len(),
                            "RENAMENX returned an unexpected reply"
                        );
                        this.pending_notification =
                            Some(Notification::error("重命名失败：服务端应答异常").autohide(true));
                    }
                    Err(e) => {
                        tracing::error!(
                            operation = "redis_key_rename",
                            connection_id = %config.id,
                            db,
                            old_key_bytes = old.len(),
                            new_key_bytes = new.len(),
                            error = %e,
                            "rename key failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("重命名失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn delete_key_op(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let r = svc.delete_key(&config, db, &key).await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                if !this.operation_context_matches(&config, db) || !current_mutation {
                    this.pending_notification = Some(match &r {
                        Ok(_) => Notification::success(format!(
                            "已在发起时的 DB {db} 删除 key {}；当前树状态已变化，未自动刷新",
                            truncate_label(&key, 60)
                        ))
                        .autohide(true),
                        Err(error) => Notification::error(
                            error.write_hint(&format!("发起时的 DB {db} 删除 key 失败")),
                        )
                        .autohide(true),
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(_) => {
                        // 本地移除即可，无需整库重扫
                        this.keys.retain(|k| k.key != key);
                        this.rebuild_tree();
                        if this.selected.as_deref() == Some(key.as_str()) {
                            this.selected = None;
                        }
                        this.pending_notification = Some(
                            Notification::success(format!(
                                "已删除 key {}",
                                truncate_label(&key, 60)
                            ))
                            .autohide(true),
                        );
                        cx.emit(KeyTreeEvent::KeysDeleted(DeletedScope::Key(key.clone())));
                    }
                    Err(e) => {
                        tracing::error!(
                            operation = "redis_key_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            error = %e,
                            "delete key from tree failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("删除失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn delete_prefix_op(&mut self, prefix: String, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let db = self.db;
        let pattern = format!("{}:*", escape_glob(&prefix));
        cx.spawn(async move |this, cx| {
            let result = delete_by_pattern(&svc, &config, db, &pattern).await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                if !this.operation_context_matches(&config, db) || !current_mutation {
                    this.pending_notification = Some(match &result {
                        Ok(count) => Notification::success(format!(
                            "已在发起时的 DB {db} 删除前缀 {} 下 {count} 个 key；当前树状态已变化，未自动刷新",
                            truncate_label(&prefix, 60)
                        ))
                        .autohide(true),
                        Err(error) => Notification::error(
                            error.write_hint(&format!("发起时的 DB {db} 删除前缀失败")),
                        )
                        .autohide(true),
                    });
                    cx.notify();
                    return;
                }
                match result {
                    Ok(n) => {
                        let sub_prefix = format!("{prefix}:");
                        if this
                            .selected
                            .as_deref()
                            .is_some_and(|s| s.starts_with(&sub_prefix))
                        {
                            this.selected = None;
                        }
                        this.pending_notification = Some(
                            Notification::success(format!(
                                "已删除前缀 {} 下 {n} 个 key",
                                truncate_label(&prefix, 60)
                            ))
                            .autohide(true),
                        );
                        cx.emit(KeyTreeEvent::KeysDeleted(DeletedScope::Prefix(
                            prefix.clone(),
                        )));
                        this.refresh(cx);
                    }
                    Err(e) => {
                        tracing::error!(
                            operation = "redis_prefix_delete",
                            connection_id = %config.id,
                            db,
                            pattern_bytes = pattern.len(),
                            error = %e,
                            "delete by prefix failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("删除失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn flush_db_op(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(mutation_token) = self.begin_tree_mutation(cx) else {
            return;
        };
        let svc = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let r = svc
                .execute_command(&config, db, vec!["FLUSHDB".to_string()])
                .await;
            let _ = this.update(cx, |this, cx| {
                let current_mutation = this.mutation_gate.finish(mutation_token);
                if !this.operation_context_matches(&config, db) || !current_mutation {
                    this.pending_notification = Some(match &r {
                        Ok(_) => Notification::success(format!(
                            "已清空发起时的 DB {db}；当前树状态已变化，未自动刷新"
                        ))
                        .autohide(true),
                        Err(error) => Notification::error(
                            error.write_hint(&format!("清空发起时的 DB {db} 失败")),
                        )
                        .autohide(true),
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(_) => {
                        this.selected = None;
                        this.pending_notification =
                            Some(Notification::success(format!("已清空 DB {db}")).autohide(true));
                        cx.emit(KeyTreeEvent::KeysDeleted(DeletedScope::Db));
                        this.refresh(cx);
                    }
                    Err(e) => {
                        tracing::error!(
                            operation = "redis_db_flush",
                            connection_id = %config.id,
                            db,
                            error = %e,
                            "flushdb failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("清空失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
