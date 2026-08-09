//! Redis 命令控制台与应答历史。

mod complete;
mod danger;
mod format;
mod render;
mod transcript;

use transcript::*;

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    ClickEvent, Context, Entity, IntoElement, ParentElement, Render, ScrollWheelEvent,
    SharedString, Styled, Subscription, UniformListScrollHandle, Window, div, prelude::*, px,
    uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState, MoveDown, MoveUp},
    v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{ConnectionConfig, RedisValue, validate_redis_command};
use ramag_domain::error::READ_ONLY_MESSAGE;
use ramag_ui::{AxisScrollGesture, RestrictScrollToAxisExt as _};
use tracing::{error, info};

use crate::views::value_display::{DISPLAY_CONTENT_WIDTH_PX, split_display_lines};

struct Entry {
    id: u64,
    command: String,
    db: u8,
    outcome: Outcome,
    display_lines: usize,
    elapsed_ms: u128,
    /// 应答超限被分段时保留原始值（driver 已整体拉回内存），供「继续展开」续格式化
    raw: Option<Arc<RedisValue>>,
    /// 续展开游标（标量=字节偏移，顶层容器=元素索引）；None = 已全部展开
    cursor: Option<usize>,
}

enum Outcome {
    Pending,
    /// 成功应答：已按显示行硬切好的行数组，供 uniform_list 等高行虚拟化
    Ok(Arc<Vec<SharedString>>),
    Err(String),
}

/// 扁平等高行，避免大应答生成完整元素树。
enum TranscriptRow {
    Header {
        command: SharedString,
        meta: SharedString,
    },
    Body {
        line: SharedString,
        tone: LineTone,
    },
    Continue {
        entry_id: u64,
        hint: SharedString,
    },
    Spacer,
}

#[derive(Clone, Copy)]
enum LineTone {
    Normal,
    Muted,
    Accent,
    Error,
}

fn tone_of(line: &str) -> LineTone {
    if line.contains("(integer)") || line.contains("(double)") || line.contains("(boolean)") {
        LineTone::Accent
    } else if line.contains("(nil)") || line.contains("(empty)") {
        LineTone::Muted
    } else {
        LineTone::Normal
    }
}

fn wrap_display_lines(raw_lines: Vec<String>) -> Vec<SharedString> {
    let mut lines: Vec<SharedString> = Vec::new();
    for line in raw_lines {
        lines.extend(split_display_lines(&line));
    }
    if lines.is_empty() {
        lines.push(SharedString::default());
    }
    lines
}

fn remaining_hint(raw: &RedisValue, cursor: usize) -> String {
    fn bytes_text(remaining: usize) -> String {
        if remaining >= 1024 * 1024 {
            format!("剩余 {:.1} MiB", remaining as f64 / 1024.0 / 1024.0)
        } else {
            format!("剩余 {} KiB", remaining.div_ceil(1024))
        }
    }
    match raw {
        RedisValue::Text(s) => bytes_text(s.len().saturating_sub(cursor)),
        RedisValue::Bytes(b) => bytes_text(b.len().saturating_sub(cursor)),
        RedisValue::List(items) | RedisValue::Set(items) | RedisValue::Array(items) => {
            format!("剩余 {} 项", items.len().saturating_sub(cursor))
        }
        RedisValue::Hash(pairs) => format!("剩余 {} 项", pairs.len().saturating_sub(cursor)),
        RedisValue::ZSet(pairs) => format!("剩余 {} 项", pairs.len().saturating_sub(cursor)),
        RedisValue::Stream(entries) => format!("剩余 {} 条", entries.len().saturating_sub(cursor)),
        _ => String::new(),
    }
}

const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_ENTRIES: usize = 100;
const MAX_TRANSCRIPT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TRANSCRIPT_LINES: usize = 200_000;
const MAX_COMMAND_HISTORY_ENTRIES: usize = 500;
const MAX_COMMAND_HISTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_PENDING_COMMANDS: usize = 8;

pub struct CliConsole {
    service: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
    history: Vec<Entry>,
    next_entry_id: u64,
    input: Entity<InputState>,
    /// 已提交命令，与应答历史分离。
    cmd_history: VecDeque<String>,
    cmd_history_bytes: usize,
    /// `None` 表示实时输入行。
    history_cursor: Option<usize>,
    transcript_rows: Vec<TranscriptRow>,
    transcript_scroll: UniformListScrollHandle,
    transcript_h_scroll: gpui::ScrollHandle,
    transcript_scroll_gesture: AxisScrollGesture,
    _subscriptions: Vec<Subscription>,
}

impl CliConsole {
    pub fn new(
        service: Arc<RedisService>,
        config: ConnectionConfig,
        db: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .validate(|value, _| value.len() <= MAX_COMMAND_BYTES)
                .placeholder("输入 Redis 命令，Enter 执行（如 GET foo）");
            state.lsp.completion_provider = Some(complete::RedisCompletionProvider::new_rc());
            state
        });
        let subs = vec![cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, e: &InputEvent, window, cx| match e {
                InputEvent::PressEnter { .. } => this.handle_submit(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        )];
        Self {
            service,
            config,
            db,
            history: Vec::new(),
            next_entry_id: 0,
            input,
            cmd_history: VecDeque::new(),
            cmd_history_bytes: 0,
            history_cursor: None,
            transcript_rows: Vec::new(),
            transcript_scroll: UniformListScrollHandle::new(),
            transcript_h_scroll: gpui::ScrollHandle::new(),
            transcript_scroll_gesture: AxisScrollGesture::default(),
            _subscriptions: subs,
        }
    }

    pub fn set_db(&mut self, db: u8, cx: &mut Context<Self>) {
        self.db = db;
        cx.notify();
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn handle_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw_len = self.input.read(cx).value().trim().len();
        if raw_len == 0 {
            return;
        }
        if raw_len > MAX_COMMAND_BYTES {
            let command = {
                let input = self.input.read(cx);
                command_preview(input.value().trim(), 200)
            };
            self.push_entry(
                command,
                Outcome::Err(format!(
                    "(error) 命令超过 {} KiB 上限，请改用专用编辑器或脚本",
                    MAX_COMMAND_BYTES / 1024
                )),
                0,
            );
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
            cx.notify();
            return;
        }
        let raw = self.input.read(cx).value().trim().to_string();
        if self.reject_if_command_queue_full(&raw, cx) {
            return;
        }
        // 被拦截或解析失败的命令也可召回修正。
        self.record_history(&raw);
        let argv = match format::tokenize(&raw) {
            Ok(a) if a.is_empty() => return,
            Ok(a) => a,
            Err(msg) => {
                self.push_entry(raw, Outcome::Err(format!("(error) 解析失败：{msg}")), 0);
                self.input.update(cx, |s, cx| s.set_value("", window, cx));
                cx.notify();
                return;
            }
        };
        if let Err(error) = validate_redis_command(&argv) {
            self.push_entry(raw, Outcome::Err(format!("(error) {}", error.message())), 0);
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
            cx.notify();
            return;
        }
        if self.config.production
            && argv
                .first()
                .is_some_and(|command| self.service.is_write_command(command))
        {
            self.push_entry(raw, Outcome::Err(format!("(error) {READ_ONLY_MESSAGE}")), 0);
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
            cx.notify();
            return;
        }
        // SELECT 会污染连接池的 DB 上下文，订阅类命令会独占连接。
        let blocked_reason = argv.first().and_then(|c| {
            let up = c.to_ascii_uppercase();
            if up == "SELECT" {
                Some("请用顶部「DB」选择器切换数据库（命令行内 SELECT 会破坏连接池的库上下文）")
            } else if matches!(
                up.as_str(),
                "MONITOR" | "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE"
            ) {
                Some("该命令会让连接卡在特殊接收模式，命令行不支持")
            } else {
                None
            }
        });
        if let Some(reason) = blocked_reason {
            self.push_entry(raw, Outcome::Err(format!("(error) {reason}")), 0);
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
            cx.notify();
            return;
        }

        // 高危命令确认时固定连接、DB 和命令，避免上下文漂移。
        if let Some(reason) = danger::dangerous_reason(&argv) {
            let preview = command_preview(&raw, 4096);
            let desc = format!(
                "目标：{} · DB {}\n命令：{preview}\n\n{reason}。确认继续吗？",
                self.config.name, self.db
            );
            let entity = cx.entity();
            let confirmed_connection_id = self.config.id.clone();
            let confirmed_db = self.db;
            let confirmed_raw = raw.clone();
            ramag_ui::open_confirm(
                "执行高危命令？",
                desc,
                "执行",
                true,
                move |window, app| {
                    entity.update(app, |this, cx| {
                        let input_changed =
                            this.input.read(cx).value().trim() != confirmed_raw.as_str();
                        if this.config.id != confirmed_connection_id
                            || this.db != confirmed_db
                            || input_changed
                        {
                            this.push_entry(
                                command_preview(&confirmed_raw, 200),
                                Outcome::Err(
                                    "(error) 连接、DB 或命令已变更，已取消执行；请重新确认".into(),
                                ),
                                0,
                            );
                            cx.notify();
                            return;
                        }
                        this.dispatch(raw, argv, window, cx);
                    });
                },
                window,
                cx,
            );
            return;
        }

        self.dispatch(raw, argv, window, cx);
    }

    fn dispatch(
        &mut self,
        raw: String,
        argv: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 高危命令确认框可能停留较久，实际执行前必须再次检查并发上限。
        if self.reject_if_command_queue_full(&raw, cx) {
            return;
        }
        let command_name = argv
            .first()
            .map(|value| value.to_ascii_uppercase())
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let command_bytes = raw.len();
        let entry_id = self.push_entry(raw, Outcome::Pending, 0);
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();

        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let start = Instant::now();
        cx.spawn(async move |this, cx| {
            let result = svc.execute_command(&config, db, argv).await;
            let elapsed = start.elapsed().as_millis();
            let _ = this.update(cx, |this, cx| {
                if let Some(entry) = this.history.iter_mut().find(|entry| entry.id == entry_id) {
                    entry.elapsed_ms = elapsed;
                    let outcome = match result {
                        Ok(v) => {
                            info!(
                                operation = "redis_command",
                                connection_id = %config.id,
                                db,
                                command = %command_name,
                                command_bytes,
                                elapsed_ms = elapsed,
                                "command completed"
                            );
                            // 保留游标，按需展开超限应答。
                            let chunk = format::lines_of_first(&v);
                            entry.cursor = chunk.cursor;
                            entry.raw = chunk.cursor.map(|_| Arc::new(v));
                            Outcome::Ok(Arc::new(wrap_display_lines(chunk.lines)))
                        }
                        Err(e) => {
                            error!(
                                operation = "redis_command",
                                connection_id = %config.id,
                                db,
                                command = %command_name,
                                command_bytes,
                                error = %e,
                                "command failed"
                            );
                            Outcome::Err(format!("(error) {}", e.message()))
                        }
                    };
                    entry.display_lines = outcome_line_count(&outcome);
                    entry.outcome = outcome;
                }
                this.prune_transcript();
                cx.notify();
            });
        })
        .detach();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        clear_completed_entries(&mut self.history);
        self.rebuild_transcript_rows();
        self.transcript_scroll_gesture.reset();
        cx.notify();
    }

    fn on_transcript_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let horizontal = self.transcript_h_scroll.clone();
        let vertical = self.transcript_scroll.0.borrow().base_handle.clone();
        ramag_ui::handle_axis_scroll(
            &mut self.transcript_scroll_gesture,
            event,
            window,
            &horizontal,
            &vertical,
            cx,
        );
    }

    fn reject_if_command_queue_full(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        if pending_command_count(&self.history) < MAX_PENDING_COMMANDS {
            return false;
        }
        self.push_entry(
            command_preview(command, 200),
            Outcome::Err(format!(
                "(error) 同时最多执行 {MAX_PENDING_COMMANDS} 条命令，请等待已有命令完成"
            )),
            0,
        );
        cx.notify();
        true
    }

    fn push_entry(&mut self, command: String, outcome: Outcome, elapsed_ms: u128) -> u64 {
        let id = self.next_entry_id;
        self.next_entry_id = self.next_entry_id.wrapping_add(1);
        let display_lines = outcome_line_count(&outcome);
        self.history.push(Entry {
            id,
            command,
            db: self.db,
            outcome,
            display_lines,
            elapsed_ms,
            raw: None,
            cursor: None,
        });
        self.prune_transcript();
        id
    }

    fn prune_transcript(&mut self) {
        prune_transcript_entries(&mut self.history);
        self.rebuild_transcript_rows();
    }

    fn rebuild_transcript_rows(&mut self) {
        let mut rows = Vec::new();
        // 最新结果紧邻输入框。
        for entry in self.history.iter().rev() {
            let meta = if matches!(entry.outcome, Outcome::Pending) {
                format!("DB {}", entry.db)
            } else {
                format!("DB {} · {} ms", entry.db, entry.elapsed_ms)
            };
            rows.push(TranscriptRow::Header {
                command: SharedString::from(format!("> {}", entry.command)),
                meta: meta.into(),
            });
            match &entry.outcome {
                Outcome::Pending => rows.push(TranscriptRow::Body {
                    line: "执行中…".into(),
                    tone: LineTone::Muted,
                }),
                Outcome::Err(message) => {
                    for line in split_display_lines(message) {
                        rows.push(TranscriptRow::Body {
                            line,
                            tone: LineTone::Error,
                        });
                    }
                }
                Outcome::Ok(lines) => {
                    for line in lines.iter() {
                        rows.push(TranscriptRow::Body {
                            tone: tone_of(line),
                            line: line.clone(),
                        });
                    }
                }
            }
            if let (Some(raw), Some(cursor)) = (&entry.raw, entry.cursor) {
                rows.push(TranscriptRow::Continue {
                    entry_id: entry.id,
                    hint: remaining_hint(raw, cursor).into(),
                });
            }
            rows.push(TranscriptRow::Spacer);
        }
        self.transcript_rows = rows;
    }

    fn continue_entry(&mut self, entry_id: u64, cx: &mut Context<Self>) {
        let Some(entry) = self.history.iter_mut().find(|entry| entry.id == entry_id) else {
            return;
        };
        let (Some(raw), Some(cursor)) = (entry.raw.clone(), entry.cursor) else {
            return;
        };
        let chunk = format::lines_of_more(&raw, cursor);
        let mut appended = wrap_display_lines(chunk.lines);
        let Outcome::Ok(lines) = &mut entry.outcome else {
            return;
        };
        let lines = Arc::make_mut(lines);
        lines.append(&mut appended);
        entry.cursor = chunk.cursor;
        if entry.cursor.is_some() && lines.len() >= MAX_TRANSCRIPT_LINES {
            entry.cursor = None;
            lines.push(SharedString::from(
                "… 已达单条展示上限，未展开部分请改用 key 详情面板查看",
            ));
        }
        if entry.cursor.is_none() {
            entry.raw = None;
        }
        entry.display_lines = outcome_line_count(&entry.outcome);
        self.prune_transcript();
        cx.notify();
    }

    fn record_history(&mut self, cmd: &str) {
        push_command_history(
            &mut self.cmd_history,
            &mut self.cmd_history_bytes,
            cmd,
            MAX_COMMAND_HISTORY_ENTRIES,
            MAX_COMMAND_HISTORY_BYTES,
        );
        self.history_cursor = None;
    }

    fn history_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = prev_cursor(self.cmd_history.len(), self.history_cursor) else {
            return;
        };
        self.history_cursor = Some(idx);
        self.apply_history_value(idx, window, cx);
    }

    fn history_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cur) = self.history_cursor else {
            return;
        };
        match next_cursor(self.cmd_history.len(), cur) {
            Some(idx) => {
                self.history_cursor = Some(idx);
                self.apply_history_value(idx, window, cx);
            }
            None => {
                self.history_cursor = None;
                self.input.update(cx, |s, cx| s.set_value("", window, cx));
            }
        }
    }

    fn apply_history_value(&self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cmd) = self.cmd_history.get(idx).cloned() else {
            return;
        };
        self.input.update(cx, |s, cx| s.set_value(cmd, window, cx));
    }
}

#[cfg(test)]
mod tests;
