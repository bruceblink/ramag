//! MongoDB 手写命令草稿跨重启恢复。结果集和树自动注入模板不落盘。

use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Window};
use ramag_domain::entities::ConnectionConfig;
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
        cx.new(|cx| {
            let mut tab = MongoQueryTab::new(service, config, database, window, cx);
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
        let Ok(json) = serde_json::to_string(&snapshot) else {
            tracing::warn!("serialize MongoDB drafts failed");
            return;
        };
        let generation = self.draft_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let generation_ref = self.draft_generation.clone();
        let executor = cx.background_executor().clone();
        let timer = executor.timer(PERSIST_DEBOUNCE);
        executor
            .spawn(async move {
                timer.await;
                if generation_ref.load(Ordering::Relaxed) != generation {
                    return;
                }
                if let Err(e) = storage.set_preference(&key, &json).await {
                    tracing::warn!(error = %e, "persist MongoDB drafts failed");
                }
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
                        tracing::warn!(error = %e, "ignore invalid MongoDB drafts");
                        None
                    }
                },
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "load MongoDB drafts failed");
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
        self.restoring_drafts = false;
        cx.notify();
    }
}
