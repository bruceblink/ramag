//! Redis 命令控制台与应答历史。

mod command;
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

use crate::views::value_display::{DISPLAY_CONTENT_WIDTH_PX, split_display_lines};
use gpui::{
    ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollWheelEvent, SharedString, Styled, Subscription, UniformListScrollHandle, Window, div,
    prelude::*, px, uniform_list,
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

struct Entry {
    id: u64,
    command: String,
    db: u8,
    outcome: Outcome,
    display_lines: usize,
    elapsed_ms: u128,
    /// 超限应答的原始值，用于继续展开。
    raw: Option<Arc<RedisValue>>,
    /// 下一段起点；None 表示已展开完。
    cursor: Option<usize>,
}

enum Outcome {
    Pending,
    /// 已按显示行切分，供虚拟列表渲染。
    Ok(Arc<Vec<SharedString>>),
    Err(String),
}

/// 均高转录行，避免大应答构建完整元素树。
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
    /// 已提交命令的历史。
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
                .placeholder("输入命令，按 Enter 执行（如 GET foo）");
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

#[cfg(test)]
#[path = "render_test.rs"]
mod render_test;
