//! 单 Tab 编辑器：JSON 命令编辑器 + 工具条 + 结果区。
//!
//! 编辑器内容是 MongoDB 原生 runCommand 风格的 JSON：
//!   `{"find": "users", "filter": {...}, "limit": 10000}` / `{"aggregate": "...", "pipeline": [...], "cursor": {}}` / `{"count": "users", "query": {...}}`
//! 运行后若返回带 `cursor.firstBatch`，自动展开为文档列表；否则把整个返回当单文档展示

mod command;

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
use ramag_domain::entities::{ConnectionConfig, MongoQueryResult};
use ramag_domain::error::DomainError;
use serde_json::Value;
use tracing::{info, warn};

use crate::actions::{FormatMongoJson, RunMongoQuery};
use crate::views::result_panel::{ResultEvent, ResultPanel};
use command::{
    command_response_kind, dangerous_command_reason, default_command_template, extract_collection,
    parse_run_command_response, truncate_chars,
};

const MAX_CONFIRM_PRETTY_BYTES: usize = 64 * 1024;

pub struct MongoQueryTab {
    pub(crate) service: Arc<MongoService>,
    pub(crate) config: ConnectionConfig,
    /// 当前默认 db；由树或连接配置同步
    pub(crate) database: String,
    /// 当前 collection（仅用于 prefill 时标记）
    pub(crate) collection: Option<String>,
    /// JSON 命令编辑器（多行）
    pub(crate) editor: Entity<InputState>,
    /// 编辑器显隐（默认 false 隐藏，与 dbclient 一致；cmd-e 切换）
    pub(crate) show_editor: bool,
    /// 结果展示
    pub(crate) result: Entity<ResultPanel>,
    pub(crate) running: bool,
    /// JSON 格式化防重入；CPU 工作在共享有界 worker 中执行。
    formatting: bool,
    /// 当前 UI 等待任务；drop 后停止等待与历史追加，旧后端回包也无法再触碰标签。
    current_task: Option<Task<()>>,
    /// 运行代际号：切库 / 切 collection / 重新运行都自增，慢查询旧回包据此丢弃，
    /// 不串到新上下文（防运行期间切换后旧结果显示在新库/集合的界面里）
    pub(crate) run_seq: u64,
    /// 待弹出的 toast（生产模式只读拦截等，render 时 push，不覆盖结果区）
    pending_notification: Option<Notification>,
    /// 上次自动注入的命令（默认模板 / 树点 collection / 示例）。编辑器内容仍等于它
    /// = 未手改，树点击可原地覆盖；否则视为手写草稿，浏览另开 Tab（防丢稿）
    last_injected_cmd: Option<String>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Debug, Clone, Copy)]
pub enum MongoQueryTabEvent {
    DraftChanged,
}

impl EventEmitter<MongoQueryTabEvent> for MongoQueryTab {}

impl MongoQueryTab {
    pub fn new(
        service: Arc<MongoService>,
        config: ConnectionConfig,
        default_db: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let database = default_db
            .or_else(|| config.database.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "admin".to_string());

        // code_editor("json") 提供 JSON 语法高亮 + 行号 + 自动缩进；命令补全挂 lsp.completion_provider
        let editor = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .code_editor("json")
                .multi_line(true)
                .line_number(true)
                .placeholder("{\"find\": \"users\", \"filter\": {}, \"limit\": 10000}")
                .default_value(default_command_template());
            state.lsp.completion_provider =
                Some(crate::completion::CommandCompletionProvider::new_rc());
            state
        });
        let result = cx.new(|cx_inner| ResultPanel::new(window, cx_inner));
        // 注入 DML 执行上下文，让结果区能增删改
        result.update(cx, |r, _| {
            r.set_context(service.clone(), config.clone(), database.clone());
        });
        // 结果区 DML 成功后请求刷新：重跑当前命令
        let refresh_sub = cx.subscribe_in(
            &result,
            window,
            |this, _, event: &ResultEvent, window, cx| match event {
                ResultEvent::Refresh => this.request_run(window, cx),
                ResultEvent::Cancel => this.cancel_if_running(cx),
            },
        );
        let editor_sub = cx.subscribe(&editor, |_this: &mut Self, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                cx.emit(MongoQueryTabEvent::DraftChanged);
            }
        });

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
            _subscriptions: vec![refresh_sub, editor_sub],
        }
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
    pub fn draft_text(&self, cx: &gpui::App) -> Option<String> {
        self.has_user_draft(cx)
            .then(|| self.editor.read(cx).value().to_string())
    }

    /// 从本地偏好恢复手写命令，不自动执行。
    pub fn restore_draft(
        &mut self,
        text: String,
        database: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(database) = database.filter(|db| !db.is_empty()) {
            self.database = database;
        }
        self.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        self.collection = None;
        self.last_injected_cmd = None;
        self.result.update(cx, |panel, _| {
            panel.set_database(self.database.clone());
            panel.set_target_collection(None);
        });
        cx.notify();
    }

    /// 由 QueryPanel 同步全局开关给新建 / 切换的 Tab
    pub fn set_show_editor(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.show_editor != v {
            self.show_editor = v;
            cx.notify();
        }
    }

    /// 用 collection 名预填一段 `find` 模板；由树点击 collection 时调
    pub fn prefill_for_collection(
        &mut self,
        database: String,
        collection: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.database = database;
        self.collection = Some(collection.clone());
        let cmd = format!(
            "{{\n  \"find\": \"{}\",\n  \"filter\": {{}},\n  \"limit\": 10000\n}}",
            collection
        );
        self.editor.update(cx, |s, cx| {
            s.set_value(cmd.clone(), window, cx);
        });
        // 树点击注入属自动内容：未手改前再点其它 collection 仍原地覆盖
        self.last_injected_cmd = Some(cmd);
        // 切 collection 是换数据源：清掉结果区残留的列 / 行过滤，避免旧过滤词串到新结果
        self.result.update(cx, |p, cx| p.clear_filters(window, cx));
        cx.notify();
    }

    /// 编辑器内容整体替换为给定命令（示例插入用，与点树 prefill 的覆盖语义一致）
    pub fn set_command(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) {
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

    /// 设置当前 db（点击树上 db 行时调）
    pub fn set_database(&mut self, db: String, cx: &mut Context<Self>) {
        if self.database != db {
            self.database = db;
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

    /// 解析并校验命令；高危操作先展示目标与风险，确认后才进入真正执行路径。
    pub fn request_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
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
                let pretty = serde_json::to_string_pretty(&cmd).unwrap_or_else(|_| text.clone());
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
        // 提取命令目标 collection + 同步当前 db，一并注入结果区作为增删改上下文。
        // self.database 切库 / 切 collection 时已更新，必须同步给结果区；否则写操作沿用 tab
        // 初始库，filter 匹配不到文档（matched 0）→ 更新 / 删除「不生效」
        let target = extract_collection(&cmd);
        self.collection = target.clone();
        let db_now = self.database.clone();
        self.result.update(cx, |p, _| {
            p.set_database(db_now);
            p.set_target_collection(target);
        });

        let svc = self.service.clone();
        let conf = self.config.clone();
        let db = self.database.clone();
        let response_kind = command_response_kind(&cmd);
        let cmd_text = text.clone();
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
            let outcome = svc.run_command(&conf, &db, cmd).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let qr: ramag_domain::error::Result<MongoQueryResult> = match outcome {
                Ok(resp) => Ok(parse_run_command_response(resp, elapsed_ms, response_kind)),
                Err(e) => Err(e),
            };
            // 写历史在同 task 顺序执行，避免 DomainError 不实现 Clone 的借用难题
            svc.append_history(&conf, cmd_text, &qr).await;

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
                        info!(
                            db = %this.database,
                            docs = r.documents.len(),
                            ms = r.elapsed_ms,
                            "mongo command done"
                        );
                        result_handle.update(cx, |p, cx| p.set_result(r, cx));
                    }
                    Err(e) => {
                        warn!(error = %e, "mongo command failed");
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

    /// 聚焦编辑器（新建 / 切换 / 关闭 Tab 后由 QueryPanel 调用，避免用户再点一下）
    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    /// 格式化编辑器 JSON
    pub fn format_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.formatting {
            self.pending_notification =
                Some(Notification::info("JSON 格式化正在进行").autohide(true));
            cx.notify();
            return;
        }
        let text = self.editor.read(cx).value().to_string();
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
                serde_json::to_string_pretty(&parsed)
                    .map_err(|error| DomainError::Other(format!("生成格式化 JSON 失败：{error}")))
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
