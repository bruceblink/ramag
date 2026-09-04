//! MongoDB 命令编辑与结果标签。

mod actions;
mod command;
mod paging;

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    v_flex,
};
use ramag_app::MongoService;
use ramag_domain::entities::{ConnectionConfig, MongoQueryResult, json_pretty_bounded};
use ramag_domain::error::DomainError;
use ramag_ui::ResultMemoryBudget;
use serde_json::Value;
use tracing::{info, warn};

use crate::actions::{FormatMongoJson, RunMongoQuery};
use crate::views::result_panel::{MongoResultPagination, ResultEvent, ResultPanel};
use crate::views::{MAX_MONGO_INTERACTIVE_INPUT_BYTES, bounded_input};
use command::{
    CommandResponseKind, command_response_kind, dangerous_command_reason, default_command_template,
    extract_collection, parse_run_command_response, truncate_chars,
};
use paging::{MongoPager, PageRequest, finish_page};

const MAX_CONFIRM_PRETTY_BYTES: usize = 64 * 1024;

pub struct MongoQueryTab {
    pub(crate) service: Arc<MongoService>,
    pub(crate) config: ConnectionConfig,
    pub(crate) database: String,
    pub(crate) collection: Option<String>,
    pub(crate) editor: Entity<InputState>,
    pub(crate) show_editor: bool,
    pub(crate) result: Entity<ResultPanel>,
    pub(crate) running: bool,
    /// 防止 JSON 格式化重入。
    formatting: bool,
    /// 当前运行任务；丢弃后旧回包不再更新标签。
    current_task: Option<Task<()>>,
    /// 运行代际号，用于丢弃切换上下文后的旧回包。
    pub(crate) run_seq: u64,
    /// 异步通知，在渲染时推送。
    pending_notification: Option<Notification>,
    /// 最近自动注入的命令，用于识别手写草稿。
    last_injected_cmd: Option<String>,
    /// `find` 分页状态。
    pager: Option<MongoPager>,
    /// 当前查询标签的页大小偏好；新查询复用上次选择的值。
    page_size: usize,
    _subscriptions: Vec<Subscription>,
}

#[derive(Debug, Clone)]
pub enum MongoQueryTabEvent {
    DraftChanged,
    CollectionImportRequested {
        db: String,
        collection: String,
        policy: ramag_domain::entities::ConflictPolicy,
        files: Vec<std::path::PathBuf>,
    },
}

impl EventEmitter<MongoQueryTabEvent> for MongoQueryTab {}

impl MongoQueryTab {
    pub fn new(
        service: Arc<MongoService>,
        config: ConnectionConfig,
        default_db: Option<String>,
        result_memory: ResultMemoryBudget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let database = default_db
            .or_else(|| config.database.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "admin".to_string());

        let editor = cx.new(|cx| {
            let mut state = bounded_input(window, cx)
                .code_editor("json")
                .multi_line(true)
                .line_number(true)
                .placeholder("{\"find\": \"users\", \"filter\": {}}")
                .default_value(default_command_template());
            state.lsp.completion_provider =
                Some(crate::completion::CommandCompletionProvider::new_rc());
            state
        });
        let result = cx.new(|cx_inner| ResultPanel::new(window, cx_inner));
        let weak_result = result.downgrade();
        let lease = result_memory.register(move |app| {
            weak_result
                .update(app, |panel, cx| panel.evict_result_for_budget(cx))
                .is_ok()
        });
        result.update(cx, |r, _| {
            r.attach_result_memory(lease);
            r.set_context(service.clone(), config.clone(), database.clone());
        });
        let refresh_sub = cx.subscribe_in(
            &result,
            window,
            |this, _, event: &ResultEvent, window, cx| match event {
                ResultEvent::Refresh => this.request_run(window, cx),
                ResultEvent::Cancel => this.cancel_if_running(cx),
                ResultEvent::PageRequested(page) => this.handle_page(*page, cx),
                ResultEvent::PageSizeChanged(page_size) => this.handle_page_size(*page_size, cx),
                ResultEvent::CollectionImportRequested {
                    db,
                    collection,
                    policy,
                    files,
                } => cx.emit(MongoQueryTabEvent::CollectionImportRequested {
                    db: db.clone(),
                    collection: collection.clone(),
                    policy: *policy,
                    files: files.clone(),
                }),
            },
        );
        let editor_for_sub = editor.clone();
        let editor_sub = cx.subscribe_in(
            &editor,
            window,
            move |this: &mut Self, _, e: &InputEvent, window, cx| {
                if !matches!(e, InputEvent::Change) {
                    return;
                }
                this.pager = None;
                if ramag_ui::clamp_multiline_input_value(
                    &editor_for_sub,
                    MAX_MONGO_INTERACTIVE_INPUT_BYTES,
                    window,
                    cx,
                ) {
                    this.pending_notification = Some(
                        Notification::warning(format!(
                            "MongoDB 编辑器最多保留 {} MiB，超出部分已截断",
                            MAX_MONGO_INTERACTIVE_INPUT_BYTES / 1024 / 1024
                        ))
                        .autohide(true),
                    );
                }
                cx.emit(MongoQueryTabEvent::DraftChanged);
            },
        );

        Self {
            service,
            config,
            database,
            collection: None,
            editor,
            show_editor: false,
            result,
            running: false,
            formatting: false,
            current_task: None,
            run_seq: 0,
            pending_notification: None,
            last_injected_cmd: Some(default_command_template()),
            pager: None,
            page_size: paging::MONGO_PAGE_SIZE,
            _subscriptions: vec![refresh_sub, editor_sub],
        }
    }

    pub fn set_result_active(&self, active: bool, cx: &mut Context<Self>) {
        self.result
            .update(cx, |result, _| result.set_result_active(active));
    }

    /// 是否存在手写草稿。
    pub fn has_user_draft(&self, cx: &gpui::App) -> bool {
        let value = self.editor.read(cx).value();
        let cur = value.trim();
        if cur.is_empty() {
            return false;
        }
        self.last_injected_cmd.as_deref().map(str::trim) != Some(cur)
    }

    /// 手写草稿快照；自动模板不落盘。
    pub fn draft_text(&self, cx: &gpui::App) -> Option<gpui::SharedString> {
        self.has_user_draft(cx)
            .then(|| self.editor.read(cx).value())
    }

    /// 恢复本地草稿，不自动执行。
    pub fn restore_draft(
        &mut self,
        text: gpui::SharedString,
        database: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.len() > MAX_MONGO_INTERACTIVE_INPUT_BYTES {
            self.result.update(cx, |panel, cx| {
                panel.set_error(
                    format!(
                        "MongoDB 草稿超过 {} MiB 安全上限，未写入编辑器",
                        MAX_MONGO_INTERACTIVE_INPUT_BYTES / 1024 / 1024
                    ),
                    cx,
                );
            });
            return;
        }
        if let Some(database) = database.filter(|db| !db.is_empty()) {
            self.database = database;
        }
        self.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        self.collection = None;
        self.last_injected_cmd = None;
        self.pager = None;
        self.result.update(cx, |panel, _| {
            panel.set_database(self.database.clone());
            panel.set_target_collection(None);
        });
        cx.notify();
    }

    pub fn set_show_editor(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.show_editor != v {
            self.show_editor = v;
            cx.notify();
        }
    }

    pub fn prefill_for_collection(
        &mut self,
        database: String,
        collection: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 使旧回包失效，避免忙碌状态阻止新集合运行。
        self.cancel_if_running(cx);
        self.database = database;
        self.collection = Some(collection.clone());
        self.pager = None;
        let cmd = find_command_template(&collection);
        self.editor.update(cx, |s, cx| {
            s.set_value(cmd.clone(), window, cx);
        });
        self.last_injected_cmd = Some(cmd);
        // 集合字段变化，清除列筛选。
        self.result
            .update(cx, |p, cx| p.clear_column_filter(window, cx));
        cx.notify();
    }

    pub fn set_command(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.pager = None;
        self.editor.update(cx, |s, cx| {
            s.set_value(cmd.to_string(), window, cx);
        });
        self.last_injected_cmd = Some(cmd.to_string());
        cx.notify();
    }

    /// 将历史命令标记为手写草稿。
    pub fn mark_user_draft(&mut self) {
        self.last_injected_cmd = None;
    }

    pub fn set_database(&mut self, db: String, cx: &mut Context<Self>) {
        if self.database != db {
            self.database = db;
            self.pager = None;
            // Mongo 无可靠 killOp 句柄，改为使旧回包失效。
            self.current_task = None;
            self.run_seq = self.run_seq.wrapping_add(1);
            self.running = false;
            self.result.update(cx, |panel, cx| {
                panel.switch_database(self.database.clone(), cx)
            });
            cx.notify();
        }
    }

    /// 集合改名后同步查询上下文并使旧结果失效。
    pub fn collection_renamed(
        &mut self,
        db: &str,
        old: &str,
        new: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.database != db || self.collection.as_deref() != Some(old) {
            return;
        }
        let auto_injected = self.last_injected_cmd.as_deref().map(str::trim)
            == Some(self.editor.read(cx).value().trim());
        self.run_seq = self.run_seq.wrapping_add(1);
        self.current_task = None;
        self.running = false;
        if auto_injected {
            self.prefill_for_collection(db.to_string(), new.to_string(), window, cx);
        } else {
            self.collection = Some(new.to_string());
        }
        self.result.update(cx, |panel, cx| {
            panel.set_database(db.to_string());
            panel.set_target_collection(Some(new.to_string()));
            panel.set_error(
                format!("集合已从 {old} 重命名为 {new}，旧结果已失效；请检查命令后重新运行"),
                cx,
            );
        });
        cx.notify();
    }

    /// 集合删除后清除结果区写入目标。
    pub fn collection_dropped(&mut self, db: &str, coll: &str, cx: &mut Context<Self>) {
        if self.database != db || self.collection.as_deref() != Some(coll) {
            return;
        }
        self.run_seq = self.run_seq.wrapping_add(1);
        self.current_task = None;
        self.running = false;
        self.collection = None;
        self.result.update(cx, |panel, cx| {
            panel.set_target_collection(None);
            panel.set_error(
                format!("集合 {db}.{coll} 已删除，旧结果与编辑入口已失效"),
                cx,
            );
        });
        cx.notify();
    }

    /// 数据库删除后切换至 admin 并使旧结果失效。
    pub fn database_dropped(&mut self, db: &str, cx: &mut Context<Self>) {
        if self.database != db {
            return;
        }
        self.run_seq = self.run_seq.wrapping_add(1);
        self.current_task = None;
        self.running = false;
        self.database = "admin".to_string();
        self.collection = None;
        self.pager = None;
        self.result.update(cx, |panel, cx| {
            panel.switch_database("admin".to_string(), cx);
            panel.set_error(format!("数据库 {db} 已删除，旧结果与编辑入口已失效"), cx);
        });
        cx.notify();
    }
}

fn find_command_template(collection: &str) -> String {
    let collection =
        serde_json::to_string(collection).unwrap_or_else(|_| "\"invalid collection\"".to_string());
    format!("{{\n  \"find\": {collection},\n  \"filter\": {{}},\n  \"sort\": {{\"_id\": 1}}\n}}")
}

impl Render for MongoQueryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            ramag_ui::push_responsive_notification(window, n, cx);
        }
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;

        let show_editor = self.show_editor;
        let editor_clone = self.editor.clone();

        v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .key_context("MongoQueryTab")
            .on_action(
                cx.listener(|this, _: &RunMongoQuery, window, cx| this.request_run(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FormatMongoJson, window, cx| this.format_json(window, cx)),
            )
            .when(show_editor, move |v| {
                v.child(
                    div()
                        .h(px(220.0))
                        .flex_none()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            Input::new(&editor_clone)
                                .h_full()
                                .bordered(false)
                                .focus_bordered(false),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(self.result.clone()))
    }
}

#[cfg(test)]
mod tests;
