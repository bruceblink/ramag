//! 单 Tab 编辑器：JSON 命令编辑器 + 工具条 + 结果区。
//!
//! 编辑器内容是 MongoDB 原生 runCommand 风格的 JSON：
//!   `{"find": "users", "filter": {...}, "limit": 10000}` / `{"aggregate": "...", "pipeline": [...], "cursor": {}}` / `{"count": "users", "query": {...}}`
//! 运行后若返回带 `cursor.firstBatch`，自动展开为文档列表；否则把整个返回当单文档展示

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, WindowExt as _,
    input::{Input, InputState},
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
        let refresh_sub = cx.subscribe(&result, |this, _, _e: &ResultEvent, cx| {
            this.run(cx);
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
            run_seq: 0,
            pending_notification: None,
            // 新 Tab 出生自带默认模板，属自动注入（未手改前树点击可原地覆盖）
            last_injected_cmd: Some(default_command_template()),
            _subscriptions: vec![refresh_sub],
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

    /// 设置当前 db（点击树上 db 行时调）
    pub fn set_database(&mut self, db: String, cx: &mut Context<Self>) {
        if self.database != db {
            self.database = db;
            cx.notify();
        }
    }

    /// 运行：编辑器内容解析为 JSON 命令 → run_command → 智能解包 cursor.firstBatch
    pub fn run(&mut self, cx: &mut Context<Self>) {
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
        let cmd_text = text.clone();
        self.running = true;
        // 代际推进 + 记录本次运行的 db（回包时比对，防运行期间切库导致串台）
        self.run_seq = self.run_seq.wrapping_add(1);
        let request_seq = self.run_seq;
        let request_db = self.database.clone();
        self.result.update(cx, |p, cx| p.set_running(cx));
        let result_handle = self.result.clone();

        cx.spawn(async move |this, cx| {
            let start = Instant::now();
            let outcome = svc.run_command(&conf, &db, cmd).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let qr: ramag_domain::error::Result<MongoQueryResult> = match outcome {
                Ok(resp) => Ok(parse_run_command_response(resp, elapsed_ms)),
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
                        // 生产模式只读拦截：弹 toast 保留结果区原有内容；其余错误仍进结果区便于排查
                        if matches!(e, DomainError::Forbidden(_)) {
                            this.pending_notification =
                                Some(Notification::warning(e.to_string()).autohide(true));
                        } else {
                            result_handle.update(cx, |p, cx| p.set_error(e.to_string(), cx));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 聚焦编辑器（新建 / 切换 / 关闭 Tab 后由 QueryPanel 调用，避免用户再点一下）
    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    /// 格式化编辑器 JSON
    pub fn format_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                self.result.update(cx, |p, cx| {
                    p.set_error(format!("格式化失败（JSON 无效）：{e}"), cx);
                });
                return;
            }
        };
        if let Ok(pretty) = serde_json::to_string_pretty(&parsed) {
            self.editor.update(cx, |s, cx| {
                s.set_value(pretty, window, cx);
            });
            cx.notify();
        }
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
            .on_action(cx.listener(|this, _: &RunMongoQuery, _, cx| this.run(cx)))
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

/// 从 runCommand JSON 提取目标 collection（find/aggregate/insert 等命令名的字符串值）
fn extract_collection(cmd: &Value) -> Option<String> {
    const CMD_KEYS: &[&str] = &[
        "find",
        "aggregate",
        "count",
        "distinct",
        "insert",
        "update",
        "delete",
        "findAndModify",
    ];
    for key in CMD_KEYS {
        if let Some(c) = cmd.get(*key).and_then(|v| v.as_str()) {
            return Some(c.to_string());
        }
    }
    None
}

/// 默认编辑器模板（无 collection 时显示）
fn default_command_template() -> String {
    "{\n  \"ping\": 1\n}".to_string()
}

/// 解析 run_command 返回：智能识别 cursor.firstBatch / 普通文档 / 错误结构
fn parse_run_command_response(response: Value, elapsed_ms: u64) -> MongoQueryResult {
    // infra 层塞的截断标记（游标超上限只取前 N），提取后据此提示用户
    let truncated = response
        .get("__ramag_truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // 优先：cursor.firstBatch（find / aggregate / listCollections / listIndexes）
    if let Some(batch) = response
        .get("cursor")
        .and_then(|c| c.get("firstBatch"))
        .and_then(|b| b.as_array())
        .cloned()
    {
        return MongoQueryResult::read_maybe_truncated(batch, elapsed_ms, truncated);
    }
    // 次优：count 返回 `n`
    if let Some(n) = response.get("n").and_then(|v| v.as_u64()) {
        return MongoQueryResult {
            documents: vec![response.clone()],
            affected: n,
            elapsed_ms,
            summary: format!("count={n}, {elapsed_ms}ms"),
            truncated: false,
        };
    }
    // 写命令：insert/update/delete 直接看 n / nModified
    if let Some(modified) = response.get("nModified").and_then(|v| v.as_u64()) {
        return MongoQueryResult::write(modified, elapsed_ms, "update");
    }
    // 兜底：整个 response 当单文档
    MongoQueryResult::read(vec![response], elapsed_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_cursor_firstbatch() {
        let resp = json!({
            "cursor": {
                "firstBatch": [{"a": 1}, {"a": 2}],
                "id": 0,
                "ns": "db.coll"
            },
            "ok": 1.0
        });
        let r = parse_run_command_response(resp, 10);
        assert_eq!(r.documents.len(), 2);
    }

    #[test]
    fn parse_count_returns_n() {
        let resp = json!({"n": 42, "ok": 1.0});
        let r = parse_run_command_response(resp, 10);
        assert_eq!(r.affected, 42);
        assert!(r.summary.contains("count=42"));
    }

    #[test]
    fn parse_unknown_falls_back_to_single_doc() {
        let resp = json!({"ok": 1.0, "value": "x"});
        let r = parse_run_command_response(resp, 5);
        assert_eq!(r.documents.len(), 1);
    }
}
