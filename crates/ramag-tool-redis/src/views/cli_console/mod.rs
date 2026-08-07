//! Redis 命令控制台与应答历史。

mod complete;
mod danger;
mod format;

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
                            info!(elapsed_ms = elapsed, "command completed");
                            // 保留游标，按需展开超限应答。
                            let chunk = format::lines_of_first(&v);
                            entry.cursor = chunk.cursor;
                            entry.raw = chunk.cursor.map(|_| Arc::new(v));
                            Outcome::Ok(Arc::new(wrap_display_lines(chunk.lines)))
                        }
                        Err(e) => {
                            error!(error = %e, "command failed");
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

impl Render for CliConsole {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let bg = theme.background;
        let secondary_bg = theme.secondary;
        let accent = theme.primary;
        let read_only_write = if self.config.production {
            let input = self.input.read(cx);
            let input_value = input.value();
            let value = input_value.trim();
            value.len() <= MAX_COMMAND_BYTES
                && format::tokenize(value)
                    .ok()
                    .and_then(|argv| argv.into_iter().next())
                    .is_some_and(|command| self.service.is_write_command(&command))
        } else {
            false
        };
        let pending_commands = pending_command_count(&self.history);
        let command_queue_full = pending_commands >= MAX_PENDING_COMMANDS;
        let history_label = if pending_commands == 0 {
            format!("命令行 · DB {} · {} 条", self.db, self.history.len())
        } else {
            format!(
                "命令行 · DB {} · {} 条 · {pending_commands} 执行中",
                self.db,
                self.history.len()
            )
        };

        let toolbar = h_flex()
            .w_full()
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .bg(secondary_bg)
            .gap(px(8.0))
            .items_center()
            .child(div().text_xs().text_color(muted_fg).child(history_label))
            .when(self.config.production, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(gpui::red())
                        .child("只读：写命令已禁用"),
                )
            })
            .child(div().flex_1())
            .child(
                ramag_ui::clickable_button("cli-clear")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::trash())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear(cx))),
            );

        let transcript: gpui::AnyElement = if self.history.is_empty() {
            div()
                .p(px(12.0))
                .text_sm()
                .text_color(muted_fg)
                .child(
                    "尚无命令；输入并 Enter 执行（PING / GET foo / KEYS * / CONFIG GET maxmemory）",
                )
                .into_any_element()
        } else {
            div()
                .relative()
                .size_full()
                .child(
                    div()
                        .id("cli-transcript-hscroll")
                        .debug_selector(|| "cli-transcript-scroll-region".into())
                        .size_full()
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&self.transcript_h_scroll)
                        .child(
                            uniform_list(
                                "cli-transcript",
                                self.transcript_rows.len(),
                                cx.processor(move |this, range: Range<usize>, _w, cx| {
                                    range
                                        .filter_map(|index| {
                                            let row = this.transcript_rows.get(index)?;
                                            Some(render_transcript_row(
                                                row, fg, muted_fg, accent, cx,
                                            ))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&self.transcript_scroll)
                            .restrict_scroll_to_axis()
                            .h_full()
                            .w(px(DISPLAY_CONTENT_WIDTH_PX)),
                        ),
                )
                .child(
                    div()
                        .id("cli-transcript-scroll-input")
                        .absolute()
                        .inset_0()
                        .on_scroll_wheel(cx.listener(Self::on_transcript_scroll)),
                )
                .into_any_element()
        };

        let input_row = h_flex()
            .w_full()
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .gap(px(8.0))
            .items_center()
            .child(div().text_xs().text_color(muted_fg).child("⏵"))
            .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
            .child(
                ramag_ui::clickable_button("cli-run")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .disabled(read_only_write || command_queue_full)
                    .when(read_only_write || command_queue_full, |button| {
                        button.tooltip(if read_only_write {
                            "只读"
                        } else {
                            "命令队列已满"
                        })
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.handle_submit(window, cx)
                    })),
            );

        v_flex()
            .size_full()
            .occlude()
            .bg(bg)
            // 输入组件不直接处理上下键，先交给补全菜单。
            .on_action(cx.listener(|this, _: &MoveUp, window, cx| {
                let handled = this.input.update(cx, |state, cx| {
                    state.handle_action_for_context_menu(Box::new(MoveUp), window, cx)
                });
                if !handled {
                    this.history_prev(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &MoveDown, window, cx| {
                let handled = this.input.update(cx, |state, cx| {
                    state.handle_action_for_context_menu(Box::new(MoveDown), window, cx)
                });
                if !handled {
                    this.history_next(window, cx);
                }
            }))
            .child(toolbar)
            .child(input_row)
            .child(div().flex_1().min_h_0().child(transcript))
    }
}

const ROW_H: f32 = 20.0;

fn render_transcript_row(
    row: &TranscriptRow,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<CliConsole>,
) -> gpui::AnyElement {
    match row {
        TranscriptRow::Continue { entry_id, hint } => {
            let entry_id = *entry_id;
            h_flex()
                .h(px(ROW_H))
                .w_full()
                .px(px(12.0))
                .items_center()
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "cli-continue-{entry_id}"
                    )))
                    .ghost()
                    .xsmall()
                    .label("继续")
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.continue_entry(entry_id, cx);
                        },
                    )),
                )
                .child(div().text_xs().text_color(muted_fg).child(hint.clone()))
                .into_any_element()
        }
        TranscriptRow::Header { command, meta } => div()
            .h(px(ROW_H))
            .w_full()
            .px(px(12.0))
            .whitespace_nowrap()
            .text_xs()
            .text_color(muted_fg)
            .font_family("monospace")
            .child(SharedString::from(format!("{command} · {meta}")))
            .into_any_element(),
        TranscriptRow::Body { line, tone } => {
            let color = match tone {
                LineTone::Normal => fg,
                LineTone::Muted => muted_fg,
                LineTone::Accent => accent,
                LineTone::Error => gpui::red(),
            };
            div()
                .h(px(ROW_H))
                .w_full()
                .px(px(12.0))
                .whitespace_nowrap()
                .text_sm()
                .text_color(color)
                .font_family("monospace")
                .child(line.clone())
                .into_any_element()
        }
        TranscriptRow::Spacer => div().h(px(ROW_H)).w_full().into_any_element(),
    }
}

fn transcript_bytes(entries: &[Entry]) -> usize {
    entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(transcript_entry_bytes(entry))
    })
}

fn prune_transcript_entries(entries: &mut Vec<Entry>) {
    let mut total_bytes = transcript_bytes(entries);
    let mut total_lines = transcript_line_count(entries);
    while entries.len() > MAX_TRANSCRIPT_ENTRIES
        || total_bytes > MAX_TRANSCRIPT_BYTES
        || total_lines > MAX_TRANSCRIPT_LINES
    {
        let Some(index) = entries
            .iter()
            .position(|entry| !matches!(entry.outcome, Outcome::Pending))
        else {
            break;
        };
        // 最新一条（当前结果）不因预算清除：单条超限时保留它、停止修剪
        if index + 1 == entries.len() {
            break;
        }
        let removed = entries.remove(index);
        total_bytes = total_bytes.saturating_sub(transcript_entry_bytes(&removed));
        total_lines = total_lines.saturating_sub(removed.display_lines);
    }
}

fn transcript_entry_bytes(entry: &Entry) -> usize {
    let outcome_bytes = match &entry.outcome {
        Outcome::Pending => 0,
        Outcome::Ok(lines) => lines.iter().map(|line| line.len()).sum(),
        Outcome::Err(value) => value.len(),
    };
    let raw_bytes = entry
        .raw
        .as_deref()
        .map(redis_value_retained_bytes)
        .unwrap_or_default();
    entry
        .command
        .len()
        .saturating_add(outcome_bytes)
        .saturating_add(raw_bytes)
}

/// 估算续展开原始结果实际持有的载荷；容器开销不计也不会低估大值的主体数据。
fn redis_value_retained_bytes(value: &RedisValue) -> usize {
    match value {
        RedisValue::Nil | RedisValue::Int(_) | RedisValue::Float(_) | RedisValue::Bool(_) => 0,
        RedisValue::Text(value) => value.len(),
        RedisValue::Bytes(value) => value.len(),
        RedisValue::List(values) | RedisValue::Set(values) | RedisValue::Array(values) => {
            values.iter().fold(0usize, |total, value| {
                total.saturating_add(redis_value_retained_bytes(value))
            })
        }
        RedisValue::Hash(pairs) => pairs.iter().fold(0usize, |total, (key, value)| {
            total
                .saturating_add(key.len())
                .saturating_add(redis_value_retained_bytes(value))
        }),
        RedisValue::ZSet(pairs) => pairs.iter().fold(0usize, |total, (member, _)| {
            total.saturating_add(redis_value_retained_bytes(member))
        }),
        RedisValue::Stream(entries) => entries.iter().fold(0usize, |total, entry| {
            entry.fields.iter().fold(
                total.saturating_add(entry.id.len()),
                |entry_total, (field, value)| {
                    entry_total
                        .saturating_add(field.len())
                        .saturating_add(value.len())
                },
            )
        }),
    }
}

fn transcript_line_count(entries: &[Entry]) -> usize {
    entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(entry.display_lines)
    })
}

fn outcome_line_count(outcome: &Outcome) -> usize {
    match outcome {
        Outcome::Pending => 1,
        Outcome::Ok(lines) => lines.len().max(1),
        Outcome::Err(value) => value.lines().count().max(1),
    }
}

fn pending_command_count(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter(|entry| matches!(entry.outcome, Outcome::Pending))
        .count()
}

fn clear_completed_entries(entries: &mut Vec<Entry>) {
    entries.retain(|entry| matches!(entry.outcome, Outcome::Pending));
}

fn push_command_history(
    history: &mut VecDeque<String>,
    total_bytes: &mut usize,
    command: &str,
    max_entries: usize,
    max_bytes: usize,
) {
    if history.back().map(String::as_str) == Some(command) {
        return;
    }

    *total_bytes = total_bytes.saturating_add(command.len());
    history.push_back(command.to_string());
    while history.len() > max_entries || *total_bytes > max_bytes {
        let Some(removed) = history.pop_front() else {
            *total_bytes = 0;
            break;
        };
        *total_bytes = total_bytes.saturating_sub(removed.len());
    }
}

fn command_preview(command: &str, max_chars: usize) -> String {
    let mut chars = command.chars();
    let mut preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        preview.push_str(&format!("…（共 {} bytes）", command.len()));
    }
    preview
}

fn prev_cursor(len: usize, cur: Option<usize>) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match cur {
        None => len - 1,
        Some(0) => 0,
        Some(i) => i - 1,
    })
}

fn next_cursor(len: usize, cur: usize) -> Option<usize> {
    if cur + 1 < len { Some(cur + 1) } else { None }
}

#[cfg(test)]
mod tests {
    use super::{
        Entry, MAX_TRANSCRIPT_ENTRIES, MAX_TRANSCRIPT_LINES, Outcome, clear_completed_entries,
        command_preview, next_cursor, outcome_line_count, pending_command_count, prev_cursor,
        prune_transcript_entries, push_command_history, redis_value_retained_bytes,
        split_display_lines, transcript_line_count,
    };
    use std::collections::VecDeque;

    fn ok_lines(text: &str) -> Outcome {
        Outcome::Ok(std::sync::Arc::new(split_display_lines(text)))
    }

    #[test]
    fn prev_from_live_jumps_to_newest() {
        // 实时行首按 ↑ → 最新一条（末尾）
        assert_eq!(prev_cursor(3, None), Some(2));
    }

    #[test]
    fn prev_walks_back_and_stops_at_oldest() {
        assert_eq!(prev_cursor(3, Some(2)), Some(1));
        assert_eq!(prev_cursor(3, Some(1)), Some(0));
        assert_eq!(prev_cursor(3, Some(0)), Some(0)); // 到最旧停住，不越界
    }

    #[test]
    fn prev_empty_history_is_noop() {
        assert_eq!(prev_cursor(0, None), None);
    }

    #[test]
    fn next_walks_forward_then_returns_to_live() {
        assert_eq!(next_cursor(3, 0), Some(1));
        assert_eq!(next_cursor(3, 1), Some(2));
        assert_eq!(next_cursor(3, 2), None); // 越过最新 → 回到实时行
    }

    #[test]
    fn pruning_never_removes_the_latest_entry_even_over_budget() {
        // 续展开可让单条超总行数预算；最新一条（当前结果）必须保留
        let outcome = ok_lines("x");
        let mut entries = vec![Entry {
            id: 1,
            command: "GET big".into(),
            db: 0,
            display_lines: MAX_TRANSCRIPT_LINES + 10,
            outcome,
            elapsed_ms: 1,
            raw: None,
            cursor: None,
        }];
        prune_transcript_entries(&mut entries);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn transcript_pruning_is_bounded_and_preserves_pending_entries() {
        let mut entries: Vec<_> = (0..=MAX_TRANSCRIPT_ENTRIES)
            .map(|id| Entry {
                id: id as u64,
                command: "PING".into(),
                db: 0,
                outcome: ok_lines("PONG"),
                display_lines: 1,
                elapsed_ms: 1,
                raw: None,
                cursor: None,
            })
            .collect();
        entries[0].outcome = Outcome::Pending;

        prune_transcript_entries(&mut entries);

        assert_eq!(entries.len(), MAX_TRANSCRIPT_ENTRIES);
        assert!(entries.iter().any(|entry| entry.id == 0));
    }

    #[test]
    fn transcript_pruning_bounds_total_rendered_lines() {
        let line_count = MAX_TRANSCRIPT_LINES / 2 + 1;
        let payload = std::iter::repeat_n("x", line_count)
            .collect::<Vec<_>>()
            .join("\n");
        let mut entries: Vec<_> = (0..3)
            .map(|id| {
                let outcome = ok_lines(&payload);
                Entry {
                    id,
                    command: "LRANGE queue 0 -1".into(),
                    db: 0,
                    display_lines: outcome_line_count(&outcome),
                    outcome,
                    elapsed_ms: 1,
                    raw: None,
                    cursor: None,
                }
            })
            .collect();

        prune_transcript_entries(&mut entries);

        assert!(transcript_line_count(&entries) <= MAX_TRANSCRIPT_LINES);
        assert_eq!(entries.last().map(|entry| entry.id), Some(2));
    }

    #[test]
    fn command_history_prunes_from_front_with_incremental_byte_count() {
        let mut history = VecDeque::new();
        let mut total_bytes = 0;

        push_command_history(&mut history, &mut total_bytes, "GET a", 2, 11);
        push_command_history(&mut history, &mut total_bytes, "GET b", 2, 11);
        push_command_history(&mut history, &mut total_bytes, "GET c", 2, 11);

        assert_eq!(history.into_iter().collect::<Vec<_>>(), ["GET b", "GET c"]);
        assert_eq!(total_bytes, 10);
    }

    #[test]
    fn command_history_skips_adjacent_duplicates() {
        let mut history = VecDeque::new();
        let mut total_bytes = 0;

        push_command_history(&mut history, &mut total_bytes, "PING", 10, 100);
        push_command_history(&mut history, &mut total_bytes, "PING", 10, 100);

        assert_eq!(history.len(), 1);
        assert_eq!(total_bytes, 4);
    }

    #[test]
    fn pending_command_count_only_counts_in_flight_entries() {
        let entries = vec![
            Entry {
                id: 1,
                command: "PING".into(),
                db: 0,
                outcome: Outcome::Pending,
                display_lines: 1,
                elapsed_ms: 0,
                raw: None,
                cursor: None,
            },
            Entry {
                id: 2,
                command: "GET a".into(),
                db: 0,
                outcome: ok_lines("x"),
                display_lines: 1,
                elapsed_ms: 1,
                raw: None,
                cursor: None,
            },
        ];

        assert_eq!(pending_command_count(&entries), 1);
    }

    #[test]
    fn clearing_transcript_preserves_in_flight_entries() {
        let mut entries = vec![
            Entry {
                id: 1,
                command: "BLPOP queue 10".into(),
                db: 0,
                outcome: Outcome::Pending,
                display_lines: 1,
                elapsed_ms: 0,
                raw: None,
                cursor: None,
            },
            Entry {
                id: 2,
                command: "PING".into(),
                db: 0,
                outcome: ok_lines("PONG"),
                display_lines: 1,
                elapsed_ms: 1,
                raw: None,
                cursor: None,
            },
        ];

        clear_completed_entries(&mut entries);

        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].outcome, Outcome::Pending));
    }

    #[test]
    fn command_preview_is_unicode_safe_and_visible() {
        assert_eq!(command_preview("你好世界", 2), "你好…（共 12 bytes）");
        assert_eq!(command_preview("PING", 10), "PING");
    }

    #[test]
    fn retained_bytes_counts_nested_raw_payloads() {
        let value = ramag_domain::entities::RedisValue::Hash(vec![(
            "field".into(),
            ramag_domain::entities::RedisValue::Array(vec![
                ramag_domain::entities::RedisValue::Text("value".into()),
                ramag_domain::entities::RedisValue::Bytes(vec![0; 3]),
            ]),
        )]);

        assert_eq!(redis_value_retained_bytes(&value), 13);
    }
}
