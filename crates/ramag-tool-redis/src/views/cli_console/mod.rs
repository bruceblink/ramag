//! Redis 命令行控制台：自由输入命令 + redis-cli 风格的滚动应答历史（transcript）。
//!
//! - 输入框 Enter 执行；上方滚动显示「命令 + 应答」块流，内容保留至手动清空
//! - argv 经 format::tokenize 解析（支持引号）；应答经 format::lines_of 递归格式化
//! - 写命令在生产（只读）连接由 driver 层拦截返回 Forbidden，这里按错误行展示
//! - 显隐由 RedisSession 控制（cmd-e / 工具栏图标 / 点击外部关闭），本面板只管内容

mod complete;
mod danger;
mod format;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    ClickEvent, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState, MoveDown, MoveUp},
    scroll::ScrollableElement as _,
    v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{ConnectionConfig, validate_redis_command};
use ramag_domain::error::READ_ONLY_MESSAGE;
use tracing::{error, info};

/// 单条命令 + 应答历史
struct Entry {
    id: u64,
    command: String,
    db: u8,
    outcome: Outcome,
    display_lines: usize,
    elapsed_ms: u128,
}

enum Outcome {
    Pending,
    Ok(String),
    Err(String),
}

const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_ENTRIES: usize = 100;
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRANSCRIPT_LINES: usize = 5_000;
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
    /// 已提交命令的输入历史（旧→新），供 ↑/↓ 召回；与上方应答历史 history 是两回事
    cmd_history: VecDeque<String>,
    cmd_history_bytes: usize,
    /// 当前 ↑/↓ 浏览位置：None = 停在实时输入行，Some(i) = 正显示 cmd_history[i]
    history_cursor: Option<usize>,
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
            // 命令名补全 + 语法提示
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
            _subscriptions: subs,
        }
    }

    /// 会话切 DB 时同步（应答按执行时所在 db 记录）
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
        // 记录到输入历史（含被拦截 / 解析失败的命令，便于 ↑ 召回后修正）
        self.record_history(&raw);
        // 引号解析失败：就地记错误行，不发后端
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
        // 命令行本地拦截两类会破坏复用连接的命令，就地报错不发后端：
        // - SELECT：改变底层连接 DB 却仍缓存为原 db，后续命令打错库（引导用 DB 选择器）
        // - MONITOR/SUBSCRIBE 等：会把连接卡在特殊接收模式，不可逆
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

        // 高危命令（FLUSHALL / SHUTDOWN / CONFIG SET / CLIENT KILL 等）先弹确认，
        // 明示连接名 + DB；取消则保留输入供修改
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

    /// 真正执行：登记应答历史 + 清输入 + 异步发后端（危险命令确认后也走这里）
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
                            info!(elapsed_ms = elapsed, "cli command ok");
                            Outcome::Ok(format::lines_of(&v).join("\n"))
                        }
                        Err(e) => {
                            error!(error = %e, "cli command failed");
                            // 仿 redis-cli：(error) + 纯消息体，不带「查询执行失败:」SQL 腔前缀
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
        cx.notify();
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
        });
        self.prune_transcript();
        id
    }

    fn prune_transcript(&mut self) {
        prune_transcript_entries(&mut self.history);
    }

    /// 记录一条输入历史：跳过与上一条完全相同的（避免连按重复堆积），上限 500 条防无界增长；
    /// 记录后浏览位置复位到实时行
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

    /// ↑ 召回更旧命令：从实时行首按即跳到最新一条，再按逐条往旧，至最旧停住
    fn history_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = prev_cursor(self.cmd_history.len(), self.history_cursor) else {
            return;
        };
        self.history_cursor = Some(idx);
        self.apply_history_value(idx, window, cx);
    }

    /// ↓ 走向更新命令：越过最新一条即回到空的实时输入行
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
                    .tooltip(if pending_commands > 0 {
                        "清空已完成历史（执行中的命令会保留）"
                    } else {
                        "清空历史"
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear(cx))),
            );

        let mut transcript = v_flex().w_full().gap(px(10.0)).p(px(12.0));
        if self.history.is_empty() {
            transcript = transcript.child(div().text_sm().text_color(muted_fg).child(
                "尚无命令；输入并 Enter 执行（PING / GET foo / KEYS * / CONFIG GET maxmemory）",
            ));
        } else {
            // 最新在上：刚执行的命令结果紧贴输入框下方，无需滚动
            for entry in self.history.iter().rev() {
                transcript = transcript.child(render_entry(entry, fg, muted_fg, accent, border));
            }
        }

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
                    .tooltip(if read_only_write {
                        "生产连接为只读，不能执行写命令"
                    } else if command_queue_full {
                        "并发命令已达上限，请等待已有命令完成"
                    } else {
                        "执行 (Enter)"
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.handle_submit(window, cx)
                    })),
            );

        // 顶部输入（补全朝下展开、最新结果就在其下）；下方 transcript 最新在上
        v_flex()
            .size_full()
            .bg(bg)
            // 单行输入不挂 up/down handler（gpui-component 限制），手动把 ↑/↓ 转发给补全菜单导航
            .on_action(cx.listener(|this, _: &MoveUp, window, cx| {
                // 补全菜单打开时 ↑ 交其导航；菜单关闭（未消费）时召回更旧的历史命令
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
            .child(
                // 外层 flex_1+min_h_0 给确定高度，内层 size_full+overflow 才能滚
                div()
                    .flex_1()
                    .min_h_0()
                    .child(div().size_full().overflow_y_scrollbar().child(transcript)),
            )
    }
}

fn render_entry(
    entry: &Entry,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement {
    // 应答区：成功按行分色（整数/浮点/布尔→强调色，nil/empty→弱化，余→前景）；错误整体红
    let body = match &entry.outcome {
        Outcome::Pending => div()
            .text_color(muted_fg)
            .child("执行中…")
            .into_any_element(),
        Outcome::Err(s) => div()
            .text_color(gpui::red())
            .child(s.clone())
            .into_any_element(),
        Outcome::Ok(s) => v_flex()
            .w_full()
            .children(s.lines().map(|line| {
                div()
                    .w_full()
                    .text_color(line_color(line, fg, muted_fg, accent))
                    .child(line.to_string())
            }))
            .into_any_element(),
    };
    let meta = if matches!(entry.outcome, Outcome::Pending) {
        format!("DB {}", entry.db)
    } else {
        format!("DB {} · {} ms", entry.db, entry.elapsed_ms)
    };
    v_flex()
        .id(SharedString::from(format!("cli-entry-{}", entry.id)))
        .w_full()
        .gap(px(4.0))
        .child(
            h_flex()
                .w_full()
                .gap(px(8.0))
                .text_xs()
                .text_color(muted_fg)
                .child(
                    div()
                        .font_family("monospace")
                        .child(format!("> {}", entry.command)),
                )
                .child(div().flex_1())
                .child(div().child(meta)),
        )
        .child(
            div()
                .w_full()
                .px(px(10.0))
                .py(px(6.0))
                .border_1()
                .border_color(border)
                .rounded(px(4.0))
                .text_sm()
                .font_family("monospace")
                .child(body),
        )
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
        let removed = entries.remove(index);
        total_bytes = total_bytes.saturating_sub(transcript_entry_bytes(&removed));
        total_lines = total_lines.saturating_sub(removed.display_lines);
    }
}

fn transcript_entry_bytes(entry: &Entry) -> usize {
    let outcome_bytes = match &entry.outcome {
        Outcome::Pending => 0,
        Outcome::Ok(value) | Outcome::Err(value) => value.len(),
    };
    entry.command.len().saturating_add(outcome_bytes)
}

fn transcript_line_count(entries: &[Entry]) -> usize {
    entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(entry.display_lines)
    })
}

fn outcome_line_count(outcome: &Outcome) -> usize {
    match outcome {
        Outcome::Pending => 1,
        Outcome::Ok(value) | Outcome::Err(value) => value.lines().count().max(1),
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

/// 应答单行配色：按 redis-cli 类型标记粗判
fn line_color(line: &str, fg: gpui::Hsla, muted_fg: gpui::Hsla, accent: gpui::Hsla) -> gpui::Hsla {
    if line.contains("(integer)") || line.contains("(double)") || line.contains("(boolean)") {
        accent
    } else if line.contains("(nil)") || line.contains("(empty)") {
        muted_fg
    } else {
        fg
    }
}

/// ↑ 召回时的目标光标（纯逻辑，便于测试）。`len` = 历史条数，`cur` = 当前浏览位置。
/// 返回 None 表示历史为空、无动作；Some(i) 表示定位到第 i 条：
/// 从实时行（cur=None）首按跳到最新一条，往旧逐条递减，到最旧（0）停住
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

/// ↓ 前进时的目标光标。返回 Some(i) 定位到第 i 条；返回 None 表示已越过最新一条、
/// 应回到空的实时输入行。仅在 cur 有效（正在浏览历史）时调用
fn next_cursor(len: usize, cur: usize) -> Option<usize> {
    if cur + 1 < len { Some(cur + 1) } else { None }
}

#[cfg(test)]
mod tests {
    use super::{
        Entry, MAX_TRANSCRIPT_ENTRIES, MAX_TRANSCRIPT_LINES, Outcome, clear_completed_entries,
        command_preview, next_cursor, outcome_line_count, pending_command_count, prev_cursor,
        prune_transcript_entries, push_command_history, transcript_line_count,
    };
    use std::collections::VecDeque;

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
    fn transcript_pruning_is_bounded_and_preserves_pending_entries() {
        let mut entries: Vec<_> = (0..=MAX_TRANSCRIPT_ENTRIES)
            .map(|id| Entry {
                id: id as u64,
                command: "PING".into(),
                db: 0,
                outcome: Outcome::Ok("PONG".into()),
                display_lines: 1,
                elapsed_ms: 1,
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
            .map(|id| Entry {
                id,
                command: "LRANGE queue 0 -1".into(),
                db: 0,
                display_lines: outcome_line_count(&Outcome::Ok(payload.clone())),
                outcome: Outcome::Ok(payload.clone()),
                elapsed_ms: 1,
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
            },
            Entry {
                id: 2,
                command: "GET a".into(),
                db: 0,
                outcome: Outcome::Ok("x".into()),
                display_lines: 1,
                elapsed_ms: 1,
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
            },
            Entry {
                id: 2,
                command: "PING".into(),
                db: 0,
                outcome: Outcome::Ok("PONG".into()),
                display_lines: 1,
                elapsed_ms: 1,
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
}
