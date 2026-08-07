use super::*;

impl TerminalCore {
    pub fn start(command: TerminalCommand) -> Result<Self> {
        command.validate()?;
        let dimensions = TerminalDimensions::new(DEFAULT_COLUMNS, DEFAULT_LINES);
        let window_size = dimensions.window_size(DEFAULT_CELL_WIDTH, DEFAULT_CELL_HEIGHT);
        let shared = Arc::new(SharedState::new(window_size));
        let proxy = TerminalEventProxy {
            shared: shared.clone(),
        };
        let config = terminal_config(SCROLLBACK_LINES);
        let terminal = Arc::new(FairMutex::new(Term::new(
            config,
            &dimensions,
            proxy.clone(),
        )));
        let mut env = command.env;
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        let mut options = Options {
            shell: Some(Shell::new(command.program, command.args)),
            drain_on_exit: true,
            env,
            ..Options::default()
        };
        #[cfg(target_os = "windows")]
        {
            options.escape_args = true;
        }
        #[cfg(not(target_os = "windows"))]
        let _ = &mut options;

        let pty = tty::new(&options, window_size, next_window_id())
            .map_err(|error| TerminalError(format!("启动终端 PTY 失败：{error}")))?;
        let event_loop = EventLoop::new(terminal.clone(), proxy, pty, true, false)
            .map_err(|error| TerminalError(format!("创建终端事件循环失败：{error}")))?;
        let sender = event_loop.channel();
        *shared.sender.lock() = Some(sender.clone());
        let thread = event_loop.spawn();
        Ok(Self {
            terminal,
            sender,
            thread: Some(thread),
            shared,
            closed: AtomicBool::new(false),
            input_enabled: AtomicBool::new(true),
            shutdown_complete: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }

    pub fn title(&self) -> Option<String> {
        self.shared.title.lock().clone()
    }

    pub fn exit_status(&self) -> Option<TerminalExit> {
        self.shared.exit.lock().clone()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn set_input_enabled(&self, enabled: bool) {
        self.input_enabled.store(enabled, Ordering::Release);
    }

    pub fn input_enabled(&self) -> bool {
        self.input_enabled.load(Ordering::Acquire)
    }

    pub fn shutdown_complete(&self) -> bool {
        self.shutdown_complete.load(Ordering::Acquire)
    }

    pub fn send(&self, bytes: impl Into<Cow<'static, [u8]>>) -> Result<()> {
        if self.is_closed() {
            return Err(TerminalError("终端已关闭".into()));
        }
        if !self.input_enabled() {
            return Err(TerminalError("终端输入已冻结".into()));
        }
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(TerminalError(format!(
                "单次终端输入超过 {} MiB 上限",
                MAX_INPUT_BYTES / 1024 / 1024
            )));
        }
        self.sender
            .send(Msg::Input(bytes))
            .map_err(|error| TerminalError(format!("写入终端失败：{error}")))
    }

    pub fn paste(&self, text: &str) -> Result<()> {
        if text.len() > MAX_INPUT_BYTES {
            return Err(TerminalError(format!(
                "粘贴内容超过 {} MiB 上限",
                MAX_INPUT_BYTES / 1024 / 1024
            )));
        }
        let bracketed = self
            .terminal
            .lock()
            .mode()
            .contains(TermMode::BRACKETED_PASTE);
        self.send(encode_paste(text, bracketed))
    }

    pub fn resize(
        &self,
        columns: usize,
        lines: usize,
        cell_width: u16,
        cell_height: u16,
    ) -> Result<()> {
        let dimensions = TerminalDimensions::new(columns, lines);
        let window_size = dimensions.window_size(cell_width.max(1), cell_height.max(1));
        let current = *self.shared.window_size.lock();
        let unchanged = current.num_lines == window_size.num_lines
            && current.num_cols == window_size.num_cols
            && current.cell_width == window_size.cell_width
            && current.cell_height == window_size.cell_height;
        if unchanged {
            return Ok(());
        }
        self.terminal.lock().resize(dimensions);
        *self.shared.window_size.lock() = window_size;
        self.sender
            .send(Msg::Resize(window_size))
            .map_err(|error| TerminalError(format!("调整终端尺寸失败：{error}")))?;
        self.shared.changed();
        Ok(())
    }

    pub fn scroll(&self, lines: i32) {
        self.terminal.lock().scroll_display(Scroll::Delta(lines));
        self.shared.changed();
    }

    /// 初始登录信息仅有少量行进入历史区时，将其恢复到首屏。
    pub fn reveal_short_initial_history(&self, max_lines: usize) -> bool {
        let mut terminal = self.terminal.lock();
        let history_lines = terminal.history_size();
        if history_lines == 0 || history_lines > max_lines {
            return false;
        }
        terminal.scroll_display(Scroll::Top);
        drop(terminal);
        self.shared.changed();
        true
    }

    /// 用户从历史区继续输入前，恢复到当前终端提示符。
    pub fn scroll_to_bottom(&self) {
        let mut terminal = self.terminal.lock();
        if terminal.grid().display_offset() == 0 {
            return;
        }
        terminal.scroll_display(Scroll::Bottom);
        drop(terminal);
        self.shared.changed();
    }

    pub fn start_selection(&self, row: usize, column: usize, side: Side) {
        let mut terminal = self.terminal.lock();
        let point = viewport_point(&terminal, row, column);
        terminal.selection = Some(Selection::new(SelectionType::Simple, point, side));
        drop(terminal);
        self.shared.changed();
    }

    pub fn update_selection(&self, row: usize, column: usize, side: Side) {
        let mut terminal = self.terminal.lock();
        let point = viewport_point(&terminal, row, column);
        if let Some(selection) = terminal.selection.as_mut() {
            selection.update(point, side);
        }
        drop(terminal);
        self.shared.changed();
    }

    pub fn clear_selection(&self) {
        self.terminal.lock().selection = None;
        self.shared.changed();
    }

    pub fn selected_text(&self) -> Option<String> {
        self.terminal.lock().selection_to_string()
    }

    pub fn take_clipboard_requests(&self) -> Vec<ClipboardRequest> {
        self.shared.clipboard.lock().drain(..).collect()
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        snapshot_term(&self.terminal.lock())
    }

    pub fn close(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        *self.shared.sender.lock() = None;
        self.input_enabled.store(false, Ordering::Release);
        if let Err(error) = self.sender.send(Msg::Shutdown) {
            tracing::warn!(error = %error, "shutdown terminal event loop failed");
        }
        let Some(thread) = self.thread.take() else {
            self.shutdown_complete.store(true, Ordering::Release);
            return;
        };
        let shutdown_complete = self.shutdown_complete.clone();
        let thread = Arc::new(Mutex::new(Some(thread)));
        let reaper_thread = thread.clone();
        // PTY 的析构会终止并回收子进程；放到专用回收线程，避免阻塞 GPUI。
        if let Err(error) = std::thread::Builder::new()
            .name("ramag-terminal-reaper".into())
            .spawn(move || {
                if let Some(thread) = reaper_thread.lock().take() {
                    join_terminal_thread(thread);
                }
                shutdown_complete.store(true, Ordering::Release);
            })
        {
            tracing::warn!(error = %error, "spawn terminal reaper failed");
            // 线程资源耗尽时退回同步回收；这是罕见失败路径，不能把 PTY 子进程留成孤儿。
            if let Some(thread) = thread.lock().take() {
                join_terminal_thread(thread);
            }
            self.shutdown_complete.store(true, Ordering::Release);
        }
    }
}

fn join_terminal_thread(thread: TerminalThread) {
    match thread.join() {
        Ok((event_loop, state)) => drop((event_loop, state)),
        Err(_) => tracing::warn!("terminal event loop panicked during shutdown"),
    }
}

impl Drop for TerminalCore {
    fn drop(&mut self) {
        self.close();
    }
}
