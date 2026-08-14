//! Redis 命令提交、确认与执行。

use super::*;
use tracing::{error, info};

impl CliConsole {
    pub(super) fn handle_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            self.input
                .update(cx, |state, cx| state.set_value("", window, cx));
            cx.notify();
            return;
        }
        let raw = self.input.read(cx).value().trim().to_string();
        if self.reject_if_command_queue_full(&raw, cx) {
            return;
        }
        // 被拒绝的命令也写入历史，便于修正。
        self.record_history(&raw);
        let argv = match format::tokenize(&raw) {
            Ok(argv) if argv.is_empty() => return,
            Ok(argv) => argv,
            Err(message) => {
                self.push_entry(raw, Outcome::Err(format!("(error) 解析失败：{message}")), 0);
                self.input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                cx.notify();
                return;
            }
        };
        if let Err(error) = validate_redis_command(&argv) {
            self.push_entry(raw, Outcome::Err(format!("(error) {}", error.message())), 0);
            self.input
                .update(cx, |state, cx| state.set_value("", window, cx));
            cx.notify();
            return;
        }
        if self.config.production
            && argv
                .first()
                .is_some_and(|command| self.service.is_write_command(command))
        {
            self.push_entry(raw, Outcome::Err(format!("(error) {READ_ONLY_MESSAGE}")), 0);
            self.input
                .update(cx, |state, cx| state.set_value("", window, cx));
            cx.notify();
            return;
        }
        // SELECT 会改变连接池状态，订阅命令会独占连接。
        let blocked_reason = argv.first().and_then(|command| {
            let command = command.to_ascii_uppercase();
            if command == "SELECT" {
                Some("请用顶部「DB」选择器切换数据库；SELECT 会改变连接池状态")
            } else if matches!(
                command.as_str(),
                "MONITOR" | "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE"
            ) {
                Some("该命令会占用连接进入接收模式，命令行不支持")
            } else {
                None
            }
        });
        if let Some(reason) = blocked_reason {
            self.push_entry(raw, Outcome::Err(format!("(error) {reason}")), 0);
            self.input
                .update(cx, |state, cx| state.set_value("", window, cx));
            cx.notify();
            return;
        }

        if let Some(reason) = danger::dangerous_reason(&argv) {
            let preview = command_preview(&raw, 4096);
            let description = format!(
                "目标：{} · DB {}\n命令：{preview}\n\n{reason}。确认继续吗？",
                self.config.name, self.db
            );
            let entity = cx.entity();
            let confirmed_connection_id = self.config.id.clone();
            let confirmed_db = self.db;
            let confirmed_raw = raw.clone();
            ramag_ui::open_confirm(
                "执行高危命令？",
                description,
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
                            let command = argv
                                .first()
                                .map(|value| value.to_ascii_uppercase())
                                .unwrap_or_else(|| "UNKNOWN".to_string());
                            info!(
                                operation = "redis_command_confirmation",
                                connection_id = %confirmed_connection_id,
                                db = confirmed_db,
                                command = %command,
                                reason = "context_changed",
                                "dangerous command confirmation invalidated"
                            );
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
        // 确认框停留期间，队列可能已满。
        if self.reject_if_command_queue_full(&raw, cx) {
            return;
        }
        let command_name = argv
            .first()
            .map(|value| value.to_ascii_uppercase())
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let command_bytes = raw.len();
        let entry_id = self.push_entry(raw, Outcome::Pending, 0);
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();

        let service = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let start = Instant::now();
        cx.spawn(async move |this, cx| {
            let result = service.execute_command(&config, db, argv).await;
            let elapsed_ms = start.elapsed().as_millis();
            let _ = this.update(cx, |this, cx| {
                if let Some(entry) = this.history.iter_mut().find(|entry| entry.id == entry_id) {
                    entry.elapsed_ms = elapsed_ms;
                    let outcome = match result {
                        Ok(value) => {
                            info!(
                                operation = "redis_command",
                                connection_id = %config.id,
                                db,
                                command = %command_name,
                                command_bytes,
                                elapsed_ms,
                                "command completed"
                            );
                            let chunk = format::lines_of_first(&value);
                            entry.cursor = chunk.cursor;
                            entry.raw = chunk.cursor.map(|_| Arc::new(value));
                            Outcome::Ok(Arc::new(wrap_display_lines(chunk.lines)))
                        }
                        Err(error) => {
                            error!(
                                operation = "redis_command",
                                connection_id = %config.id,
                                db,
                                command = %command_name,
                                command_bytes,
                                error = %error,
                                "command failed"
                            );
                            Outcome::Err(format!("(error) {}", error.message()))
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
}
