//! MongoDB 手写命令草稿跨重启恢复。结果集和树自动注入模板不落盘。

use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Window};
use ramag_domain::entities::ConnectionConfig;
use ramag_domain::error::DomainError;
use ramag_ui::{EditorDraftPref, EditorWorkspacePref};

use super::MongoQueryPanel;
use crate::views::query_tab::{MongoQueryTab, MongoQueryTabEvent};

const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);

impl MongoQueryPanel {
    pub(super) fn build_tab(
        &self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MongoQueryTab> {
        let service = self.service.clone();
        let database = Some(self.database.clone());
        let show_editor = self.show_editor;
        let result_memory = self.result_memory.clone();
        cx.new(|cx| {
            let mut tab = MongoQueryTab::new(service, config, database, result_memory, window, cx);
            tab.set_show_editor(show_editor, cx);
            tab
        })
    }

    fn draft_pref_key(&self) -> Option<String> {
        self.connection
            .as_ref()
            .map(|conn| format!("mongo_query_drafts_{}", conn.id))
    }

    fn draft_snapshot(&self, cx: &App) -> EditorWorkspacePref {
        let mut drafts = Vec::new();
        let mut active = 0usize;
        for (index, tab) in self.tabs.iter().enumerate() {
            let tab = tab.read(cx);
            let Some(text) = tab.draft_text(cx) else {
                continue;
            };
            if index == self.active {
                active = drafts.len();
            }
            drafts.push(EditorDraftPref {
                title: self
                    .titles
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("查询 {}", drafts.len() + 1)),
                text,
                context: Some(tab.database.clone()),
            });
        }
        EditorWorkspacePref::new(active, drafts)
    }

    pub(super) fn schedule_draft_persist(&mut self, cx: &mut Context<Self>) {
        if self.restoring_drafts {
            return;
        }
        let snapshot = self.draft_snapshot(cx);
        if self.draft_load_pending {
            if snapshot.drafts.is_empty() {
                return;
            }
            self.draft_load_pending = false;
        }
        let Some(storage) = ramag_ui::theme::storage_from_cx(cx) else {
            return;
        };
        let Some(key) = self.draft_pref_key() else {
            return;
        };
        let generation = self
            .draft_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let generation_ref = self.draft_generation.clone();
        let write_lock = self.draft_write_lock.clone();
        if let Err(error) = snapshot.validate() {
            self.draft_persist_error = Some(error);
            cx.notify();
            return;
        }
        // cx.spawn 而非 background spawn：失败要回写 draft_persist_error 供警示条展示。
        // 快照只克隆 SharedString 引用；真正的 JSON 生成推迟到防抖命中后，并在线程池执行。
        // 任务 detach 后独立运行至完成，面板关闭不影响最后一次真实落盘。
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PERSIST_DEBOUNCE).await;
            if generation_ref.load(Ordering::Relaxed) != generation {
                return;
            }
            let serialized = ramag_app::run_blocking(move || {
                snapshot.to_json().map_err(DomainError::InvalidConfig)
            })
            .await;
            let result = match serialized {
                Ok(json) => {
                    let _guard = write_lock.lock().await;
                    if generation_ref.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    storage.set_preference(&key, &json).await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = &result {
                tracing::warn!(
                    operation = "mongo_query_draft_save",
                    error = %error,
                    driver = "mongodb",
                    "persist query drafts failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                if generation_ref.load(Ordering::Relaxed) != generation {
                    return;
                }
                match &result {
                    Ok(()) => {
                        // 恢复成功后撤掉之前的失败警示
                        if this.draft_persist_error.take().is_some() {
                            cx.notify();
                        }
                    }
                    Err(e) => {
                        this.draft_persist_error = Some(e.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn load_persisted_drafts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(storage) = ramag_ui::theme::storage_from_cx(cx) else {
            return;
        };
        let Some(key) = self.draft_pref_key() else {
            return;
        };
        let expected_id = self.connection.as_ref().map(|conn| conn.id.clone());
        self.draft_load_pending = true;
        cx.spawn_in(window, async move |this, async_cx| {
            let loaded = match storage.get_preference(&key).await {
                Ok(Some(json)) => match EditorWorkspacePref::parse(&json) {
                    Ok(pref) if !pref.drafts.is_empty() => Some(pref),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!(
                            operation = "mongo_query_draft_load",
                            error = %e,
                            driver = "mongodb",
                            "ignore invalid MongoDB drafts"
                        );
                        None
                    }
                },
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(
                        operation = "mongo_query_draft_load",
                        error = %e,
                        driver = "mongodb",
                        "load query drafts failed"
                    );
                    None
                }
            };
            let _ = this.update_in(async_cx, move |this, window, cx| {
                if this.connection.as_ref().map(|conn| &conn.id) != expected_id.as_ref() {
                    return;
                }
                if !this.draft_load_pending {
                    return;
                }
                this.draft_load_pending = false;
                if let Some(pref) = loaded {
                    this.restore_drafts(pref, window, cx);
                }
            });
        })
        .detach();
    }

    fn restore_drafts(
        &mut self,
        pref: EditorWorkspacePref,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.connection.clone() else {
            self.restoring_drafts = false;
            return;
        };
        self.restoring_drafts = true;
        self.tabs.clear();
        self.titles.clear();
        self.draft_subscriptions.clear();

        for draft in pref.drafts {
            let title = if draft.title.trim().is_empty() {
                format!("查询 {}", self.tabs.len() + 1)
            } else {
                draft.title
            };
            let tab = self.build_tab(config.clone(), window, cx);
            tab.update(cx, |tab, cx| {
                tab.restore_draft(draft.text, draft.context, window, cx);
            });
            let sub = cx.subscribe(&tab, |this: &mut Self, _, e: &MongoQueryTabEvent, cx| {
                if matches!(e, MongoQueryTabEvent::DraftChanged) {
                    this.schedule_draft_persist(cx);
                }
            });
            self.tabs.push(tab);
            self.titles.push(title);
            self.draft_subscriptions.push(sub);
        }
        self.active = pref.active.min(self.tabs.len().saturating_sub(1));
        self.sync_result_activity(cx);
        self.restoring_drafts = false;
        cx.notify();
    }
}
