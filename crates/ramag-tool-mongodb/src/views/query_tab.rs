//! MongoDB `runCommand` JSON 编辑与结果标签。

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

    /// 解析并校验命令；高危操作先展示目标与风险，确认后才进入真正执行路径。
    pub fn request_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let text = self.editor.read(cx).value();
        if text.len() > ramag_ui::MAX_EDITOR_DRAFT_BYTES {
            self.result.update(cx, |panel, cx| {
                panel.set_error(
                    format!(
                        "MongoDB 命令超过 {} MiB 安全上限，无法运行；请拆分命令后重试",
                        ramag_ui::MAX_EDITOR_DRAFT_BYTES / 1024 / 1024
                    ),
                    cx,
                );
            });
            return;
        }
        let text = text.to_string();
        let cmd: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                self.result.update(cx, |p, cx| {
                    p.set_error(format!("JSON 解析失败：{e}"), cx);
                });
                return;
            }
        };
        if !cmd.is_object() {
            self.result.update(cx, |p, cx| {
                p.set_error("顶层 JSON 必须是对象".to_string(), cx);
            });
            return;
        }
        if let Some(reason) = dangerous_command_reason(&cmd) {
            let command_preview = if text.len() <= MAX_CONFIRM_PRETTY_BYTES {
                let pretty = json_pretty_bounded(&cmd, MAX_CONFIRM_PRETTY_BYTES)
                    .unwrap_or_else(|| text.clone());
                truncate_chars(&pretty, 1_000)
            } else {
                format!(
                    "{}\n\n（命令超过 {} KiB，仅展示原文前缀）",
                    truncate_chars(&text, 1_000),
                    MAX_CONFIRM_PRETTY_BYTES / 1024
                )
            };
            let description = format!(
                "连接：{}\n数据库：{}\n风险：{reason}\n\n命令：\n{command_preview}\n\n确认继续执行吗？",
                self.config.name, self.database
            );
            let confirmed_database = self.database.clone();
            let entity = cx.entity();
            ramag_ui::open_confirm(
                "执行 MongoDB 高危命令？",
                description,
                "执行",
                true,
                move |_window, app| {
                    entity.update(app, |this, cx| {
                        if this.database != confirmed_database
                            || this.editor.read(cx).value() != text
                        {
                            this.pending_notification = Some(
                                Notification::warning("数据库或命令已变更，已取消执行；请重新确认")
                                    .autohide(true),
                            );
                            cx.notify();
                            return;
                        }
                        this.run_parsed(text.clone(), cmd.clone(), cx)
                    });
                },
                window,
                cx,
            );
            return;
        }
        self.run_parsed(text, cmd, cx);
    }

    /// 真正执行已解析、已确认（如需要）的命令。
    fn run_parsed(&mut self, text: String, cmd: Value, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let response_kind = command_response_kind(&cmd);
        let (effective_command, page_request) = if let Some(pager) = MongoPager::from_command(&cmd)
        {
            let page = match pager.command_for_page(0) {
                Ok(page) => page,
                Err(message) => {
                    self.result.update(cx, |panel, cx| {
                        panel.set_error(format!("MongoDB 分页初始化失败：{message}"), cx)
                    });
                    return;
                }
            };
            self.pager = Some(pager);
            (page.0, Some(page.1))
        } else {
            self.pager = None;
            (cmd.clone(), None)
        };
        self.execute_command(
            cmd,
            effective_command,
            response_kind,
            Some(text),
            page_request,
            cx,
        );
    }

    /// 加载相邻结果页，不改写编辑器或历史。
    fn handle_page(&mut self, requested_page: usize, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(pager) = self.pager.as_ref() else {
            return;
        };
        if !pager.accepts_adjacent_page(requested_page) {
            return;
        }
        let base_command = pager.base_command().clone();
        let (effective_command, page_request) = match pager.command_for_page(requested_page) {
            Ok(page) => page,
            Err(message) => {
                self.pending_notification = Some(
                    Notification::error(format!("加载 MongoDB 分页失败：{message}")).autohide(true),
                );
                cx.notify();
                return;
            }
        };
        let response_kind = command_response_kind(&base_command);
        self.execute_command(
            base_command,
            effective_command,
            response_kind,
            None,
            Some(page_request),
            cx,
        );
    }

    /// 执行原始命令或分页命令。
    fn execute_command(
        &mut self,
        base_command: Value,
        effective_command: Value,
        response_kind: CommandResponseKind,
        history_text: Option<String>,
        page_request: Option<PageRequest>,
        cx: &mut Context<Self>,
    ) {
        // 同步命令目标与当前库，避免写操作仍使用标签初始库。
        let target = extract_collection(&base_command);
        self.collection = target.clone();
        let db_now = self.database.clone();
        self.result.update(cx, |p, _| {
            p.set_database(db_now);
            p.set_target_collection(target);
        });

        let svc = self.service.clone();
        let conf = self.config.clone();
        let db = self.database.clone();
        self.running = true;
        // 代际推进 + 记录本次运行的 db（回包时比对，防运行期间切库导致串台）
        self.run_seq = self.run_seq.wrapping_add(1);
        let request_seq = self.run_seq;
        let request_db = self.database.clone();
        // 生产只读拦截（Forbidden）时恢复用：set_running 会清掉原错误文案
        let prev_error = self.result.read(cx).error.clone();
        self.result.update(cx, |p, cx| p.set_running(cx));
        let result_handle = self.result.clone();

        let task = cx.spawn(async move |this, cx| {
            let start = Instant::now();
            let outcome = svc.run_command(&conf, &db, effective_command).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let (qr, page_has_more): (ramag_domain::error::Result<MongoQueryResult>, Option<bool>) =
                match outcome {
                    Ok(resp) => {
                        let mut result =
                            parse_run_command_response(resp, elapsed_ms, response_kind);
                        let has_more =
                            page_request.map(|request| finish_page(&mut result, request));
                        (Ok(result), has_more)
                    }
                    Err(e) => (Err(e), None),
                };
            // 写历史在同 task 顺序执行，避免 DomainError 不实现 Clone 的借用难题
            if let Some(command_text) = history_text {
                svc.append_history(&conf, command_text, &qr).await;
            }

            let _ = this.update(cx, |this, cx| {
                // 请求身份校验：切库 / 重新运行后旧回包不得覆盖新上下文的结果
                if this.run_seq != request_seq || this.database != request_db {
                    // 仅当自己仍是最新在途请求时才复位忙碌态（含结果区，否则「执行中」
                    // 永久卡在界面上）；已有更新请求在途则一概不动，避免误清新查询状态
                    if this.run_seq == request_seq {
                        this.running = false;
                        result_handle.update(cx, |p, cx| {
                            p.set_error("查询上下文已切换，本次结果已丢弃；请重新运行".into(), cx);
                        });
                    }
                    return;
                }
                this.running = false;
                this.current_task = None;
                match qr {
                    Ok(r) => {
                        let pagination =
                            page_request
                                .zip(page_has_more)
                                .and_then(|(request, has_more)| {
                                    let displayed = r.documents.len();
                                    this.pager.as_mut().map(|pager| {
                                        pager.finish_request(request, displayed, has_more);
                                        MongoResultPagination {
                                            page: request.page,
                                            page_size: request.page_size,
                                            has_more: pager.has_more,
                                        }
                                    })
                                });
                        info!(
                            db = %this.database,
                            docs = r.documents.len(),
                            ms = r.elapsed_ms,
                            "command completed"
                        );
                        result_handle.update(cx, |panel, cx| {
                            panel.set_result(r, cx);
                            if panel.result.is_some() {
                                panel.set_pagination(pagination, cx);
                            }
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "command failed");
                        // 生产模式只读拦截：弹 toast 并复位忙碌态（旧结果 / 旧错误原样恢复，
                        // 否则结果区永久停在"执行中"）；其余错误仍进结果区便于排查
                        if matches!(e, DomainError::Forbidden(_)) {
                            this.pending_notification =
                                Some(Notification::warning(e.to_string()).autohide(true));
                            result_handle.update(cx, |p, cx| p.restore_idle(prev_error, cx));
                        } else {
                            result_handle.update(cx, |p, cx| p.set_error(e.to_string(), cx));
                        }
                    }
                }
                cx.notify();
            });
        });
        self.current_task = Some(task);
    }

    /// 关闭标签前停止等待当前命令。MongoDB 暂无可靠 killOp 句柄，因此只保证客户端任务退出、
    /// 结果不再回写；服务器端命令仍由服务端超时/完成机制收尾。
    pub fn cancel_if_running(&mut self, cx: &mut Context<Self>) {
        if self.current_task.take().is_some() || self.running {
            self.run_seq = self.run_seq.wrapping_add(1);
            self.running = false;
            self.result.update(cx, |panel, cx| {
                panel.set_error("已停止等待该命令；服务器端操作可能仍在收尾".into(), cx);
            });
            cx.notify();
        }
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    pub fn format_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.formatting {
            self.pending_notification =
                Some(Notification::info("JSON 格式化正在进行").autohide(true));
            cx.notify();
            return;
        }
        let text = self.editor.read(cx).value();
        if text.len() > ramag_ui::MAX_EDITOR_DRAFT_BYTES {
            self.result.update(cx, |panel, cx| {
                panel.set_error(
                    format!(
                        "MongoDB 命令超过 {} MiB 安全上限，无法格式化；请拆分命令后重试",
                        ramag_ui::MAX_EDITOR_DRAFT_BYTES / 1024 / 1024
                    ),
                    cx,
                );
            });
            return;
        }
        if text.trim().is_empty() {
            return;
        }
        self.formatting = true;
        cx.notify();
        let source_text = text.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let formatted = ramag_app::run_blocking(move || {
                let parsed: Value = serde_json::from_str(&text).map_err(|error| {
                    DomainError::InvalidConfig(format!("格式化失败（JSON 无效）：{error}"))
                })?;
                json_pretty_bounded(&parsed, MAX_MONGO_INTERACTIVE_INPUT_BYTES).ok_or_else(|| {
                    DomainError::InvalidConfig(format!(
                        "格式化结果超过 {} MiB 安全上限",
                        MAX_MONGO_INTERACTIVE_INPUT_BYTES / 1024 / 1024
                    ))
                })
            })
            .await;
            let _ = this.update_in(async_cx, move |this, window, cx| {
                this.formatting = false;
                if this.editor.read(cx).value() != source_text {
                    this.pending_notification = Some(
                        Notification::warning("JSON 已在格式化期间发生变化，未覆盖新内容")
                            .autohide(true),
                    );
                    cx.notify();
                    return;
                }
                match formatted {
                    Ok(pretty) if pretty.len() > MAX_MONGO_INTERACTIVE_INPUT_BYTES => {
                        this.pending_notification = Some(
                            Notification::error(format!(
                                "格式化结果超过 {} MiB 安全上限，已保留原命令",
                                MAX_MONGO_INTERACTIVE_INPUT_BYTES / 1024 / 1024
                            ))
                            .autohide(true),
                        );
                    }
                    Ok(pretty) if pretty != source_text => {
                        this.editor.update(cx, |state, cx| {
                            state.set_value(pretty, window, cx);
                        });
                        cx.emit(MongoQueryTabEvent::DraftChanged);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.result.update(cx, |panel, cx| {
                            panel.set_error(error.to_string(), cx);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
mod template_tests {
    use super::find_command_template;

    #[test]
    fn collection_template_escapes_json_string_characters() {
        let template = find_command_template("quotes\"and\\slashes");
        let parsed: serde_json::Value = serde_json::from_str(&template).unwrap();
        assert_eq!(parsed["find"], "quotes\"and\\slashes");
        assert_eq!(parsed["sort"]["_id"], 1);
        assert!(parsed.get("limit").is_none());
    }
}
