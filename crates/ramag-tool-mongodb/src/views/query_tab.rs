//! MongoDB `runCommand` JSON 编辑与结果标签。

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
    ActiveTheme, WindowExt as _,
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
    /// JSON 格式化防重入；CPU 工作在共享有界 worker 中执行。
    formatting: bool,
    /// 当前 UI 等待任务；drop 后停止等待与历史追加，旧后端回包也无法再触碰标签。
    current_task: Option<Task<()>>,
    /// 运行代际号：切库 / 切 collection / 重新运行都自增，慢查询旧回包据此丢弃，
    /// 不串到新上下文（防运行期间切换后旧结果显示在新库/集合的界面里）
    pub(crate) run_seq: u64,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pending_notification: Option<Notification>,
    /// 上次自动注入的命令（默认模板 / 树点 collection / 示例）。编辑器内容仍等于它
    /// = 未手改，树点击可原地覆盖；否则视为手写草稿，浏览另开 Tab（防丢稿）
    last_injected_cmd: Option<String>,
    /// 普通 `find` 分页状态，基线命令与编辑器文本隔离。
    pager: Option<MongoPager>,
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
            // 新 Tab 出生自带默认模板，属自动注入（未手改前树点击可原地覆盖）
            last_injected_cmd: Some(default_command_template()),
            pager: None,
            _subscriptions: vec![refresh_sub, editor_sub],
        }
    }

    pub fn set_result_active(&self, active: bool, cx: &mut Context<Self>) {
        self.result
            .update(cx, |result, _| result.set_result_active(active));
    }

    /// 是否存在用户手写草稿：编辑器非空且内容不等于上次自动注入的命令
    pub fn has_user_draft(&self, cx: &gpui::App) -> bool {
        let value = self.editor.read(cx).value();
        let cur = value.trim();
        if cur.is_empty() {
            return false;
        }
        self.last_injected_cmd.as_deref().map(str::trim) != Some(cur)
    }

    /// 手写草稿快照；默认模板和树自动注入不落盘。
    pub fn draft_text(&self, cx: &gpui::App) -> Option<gpui::SharedString> {
        self.has_user_draft(cx)
            .then(|| self.editor.read(cx).value())
    }

    /// 从本地偏好恢复手写命令，不自动执行。
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
        // 连点切集合时，停止等待旧命令后立即运行新集合，不能让 request_run 因旧 busy 状态静默失效。
        self.cancel_if_running(cx);
        self.database = database;
        self.collection = Some(collection.clone());
        self.pager = None;
        let cmd = find_command_template(&collection);
        self.editor.update(cx, |s, cx| {
            s.set_value(cmd.clone(), window, cx);
        });
        // 树点击注入属自动内容：未手改前再点其它 collection 仍原地覆盖
        self.last_injected_cmd = Some(cmd);
        // collection 的列结构会变化；内容搜索作为用户条件跨集合保留。
        self.result
            .update(cx, |p, cx| p.clear_column_filter(window, cx));
        cx.notify();
    }

    pub fn set_command(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.pager = None;
        self.editor.update(cx, |s, cx| {
            s.set_value(cmd.to_string(), window, cx);
        });
        // 示例模板属自动注入：未手改前树点击仍可原地覆盖
        self.last_injected_cmd = Some(cmd.to_string());
        cx.notify();
    }

    /// 历史记录填入属于用户主动选择，后续关闭/重启都应按手写草稿保护。
    pub fn mark_user_draft(&mut self) {
        self.last_injected_cmd = None;
    }

    pub fn set_database(&mut self, db: String, cx: &mut Context<Self>) {
        if self.database != db {
            self.database = db;
            self.pager = None;
            // Mongo driver 当前没有可靠 killOp 句柄；让旧回包失效，并清除旧结果的 DML 目标。
            self.current_task = None;
            self.run_seq = self.run_seq.wrapping_add(1);
            self.running = false;
            self.result.update(cx, |panel, cx| {
                panel.switch_database(self.database.clone(), cx)
            });
            cx.notify();
        }
    }

    /// 集合改名后同步或失效旧查询上下文，防止结果区继续对旧集合执行 DML。
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

    /// 集合删除后清除结果区写入目标，保留手写命令供用户参考。
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

    /// 数据库删除后，旧结果不能继续编辑；先落到 admin，等待树选择新的业务库。
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
            window.push_notification(n, cx);
        }
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;

        // 编辑器仅在 show_editor=true 时显示；运行 / 格式化按钮已移到 query_panel 顶部 tab 栏（与 dbclient 一致）
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
