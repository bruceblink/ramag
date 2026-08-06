//! PTY 生命周期、ANSI 状态快照与终端交互核心。

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::tty::{self, Options, Pty, Shell};
use parking_lot::Mutex;

mod snapshot;

#[cfg(test)]
use snapshot::default_foreground;
use snapshot::{indexed_color, snapshot_term, viewport_point};

const DEFAULT_COLUMNS: usize = 80;
const DEFAULT_LINES: usize = 24;
const DEFAULT_CELL_WIDTH: u16 = 8;
const DEFAULT_CELL_HEIGHT: u16 = 18;
const SCROLLBACK_LINES: usize = 10_000;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_TITLE_BYTES: usize = 4096;
const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
const MAX_CLIPBOARD_EVENTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalError(pub String);

impl Display for TerminalError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TerminalError {}

pub type Result<T> = std::result::Result<T, TerminalError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

impl TerminalCommand {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            env: HashMap::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.program.is_empty() || self.program.contains('\0') {
            return Err(TerminalError("终端程序路径无效".into()));
        }
        if self.args.iter().any(|argument| argument.contains('\0')) {
            return Err(TerminalError("终端参数不能包含 NUL 字符".into()));
        }
        if self.env.iter().any(|(key, value)| {
            key.is_empty()
                || key.contains(['=', '\0'])
                || value.contains('\0')
                || key.len().saturating_add(value.len()) > 64 * 1024
        }) {
            return Err(TerminalError("终端环境变量无效或过长".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl RgbColor {
    const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub dim: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCell {
    pub text: String,
    pub foreground: RgbColor,
    pub background: RgbColor,
    pub style: TerminalStyle,
    pub selected: bool,
    pub wide_spacer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCursorShape {
    Hidden,
    Block,
    Underline,
    Beam,
    HollowBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCursor {
    pub row: usize,
    pub column: usize,
    pub shape: TerminalCursorShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub columns: usize,
    pub lines: usize,
    pub rows: Vec<Vec<TerminalCell>>,
    pub cursor: Option<TerminalCursor>,
    pub alternate_screen: bool,
    pub bracketed_paste: bool,
    pub application_cursor: bool,
    pub display_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub success: bool,
}

pub enum ClipboardRequest {
    Store(String),
    Load(Arc<dyn Fn(&str) -> String + Send + Sync>),
}

struct SharedState {
    revision: AtomicU64,
    title: Mutex<Option<String>>,
    exit: Mutex<Option<TerminalExit>>,
    clipboard: Mutex<VecDeque<ClipboardRequest>>,
    sender: Mutex<Option<EventLoopSender>>,
    window_size: Mutex<WindowSize>,
}

impl SharedState {
    fn new(window_size: WindowSize) -> Self {
        Self {
            revision: AtomicU64::new(1),
            title: Mutex::new(None),
            exit: Mutex::new(None),
            clipboard: Mutex::new(VecDeque::with_capacity(MAX_CLIPBOARD_EVENTS)),
            sender: Mutex::new(None),
            window_size: Mutex::new(window_size),
        }
    }

    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }

    fn send_input(&self, input: String) {
        if input.is_empty() || input.len() > MAX_INPUT_BYTES {
            return;
        }
        if let Some(sender) = self.sender.lock().as_ref()
            && let Err(error) = sender.send(Msg::Input(Cow::Owned(input.into_bytes())))
        {
            tracing::warn!(error = %error, "send terminal protocol response failed");
        }
    }

    fn queue_clipboard(&self, request: ClipboardRequest) {
        let mut clipboard = self.clipboard.lock();
        if clipboard.len() >= MAX_CLIPBOARD_EVENTS {
            clipboard.pop_front();
        }
        clipboard.push_back(request);
        drop(clipboard);
        self.changed();
    }
}

#[derive(Clone)]
struct TerminalEventProxy {
    shared: Arc<SharedState>,
}

impl EventListener for TerminalEventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange | Event::Bell => {
                self.shared.changed();
            }
            Event::Title(title) => {
                *self.shared.title.lock() = Some(bounded_text(title, MAX_TITLE_BYTES));
                self.shared.changed();
            }
            Event::ResetTitle => {
                *self.shared.title.lock() = None;
                self.shared.changed();
            }
            Event::ChildExit(status) => {
                *self.shared.exit.lock() = Some(exit_status(status));
                self.shared.changed();
            }
            Event::Exit => {
                if self.shared.exit.lock().is_none() {
                    *self.shared.exit.lock() = Some(TerminalExit {
                        code: None,
                        success: false,
                    });
                }
                self.shared.changed();
            }
            Event::PtyWrite(text) => self.shared.send_input(text),
            Event::ClipboardStore(_, text) => {
                self.shared
                    .queue_clipboard(ClipboardRequest::Store(bounded_text(
                        text,
                        MAX_CLIPBOARD_BYTES,
                    )));
            }
            Event::ClipboardLoad(_, formatter) => {
                self.shared
                    .queue_clipboard(ClipboardRequest::Load(formatter));
            }
            Event::ColorRequest(index, formatter) => {
                self.shared.send_input(formatter(indexed_color(index)));
            }
            Event::TextAreaSizeRequest(formatter) => {
                self.shared
                    .send_input(formatter(*self.shared.window_size.lock()));
            }
        }
    }
}

type TerminalThread = JoinHandle<(EventLoop<Pty, TerminalEventProxy>, State)>;

pub struct TerminalCore {
    terminal: Arc<FairMutex<Term<TerminalEventProxy>>>,
    sender: EventLoopSender,
    thread: Option<TerminalThread>,
    shared: Arc<SharedState>,
    closed: AtomicBool,
    input_enabled: AtomicBool,
    shutdown_complete: Arc<AtomicBool>,
}

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
        // PTY 的析构会终止并回收子进程；放到专用回收线程，避免阻塞 GPUI。
        if let Err(error) = std::thread::Builder::new()
            .name("ramag-terminal-reaper".into())
            .spawn(move || {
                match thread.join() {
                    Ok((event_loop, state)) => drop((event_loop, state)),
                    Err(_) => tracing::warn!("terminal event loop panicked during shutdown"),
                }
                shutdown_complete.store(true, Ordering::Release);
            })
        {
            tracing::warn!(error = %error, "spawn terminal reaper failed");
        }
    }
}

impl Drop for TerminalCore {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug, Clone, Copy)]
struct TerminalDimensions {
    columns: usize,
    lines: usize,
}

impl TerminalDimensions {
    fn new(columns: usize, lines: usize) -> Self {
        Self {
            columns: columns.clamp(2, u16::MAX as usize),
            lines: lines.clamp(1, u16::MAX as usize),
        }
    }

    fn window_size(self, cell_width: u16, cell_height: u16) -> WindowSize {
        WindowSize {
            num_lines: self.lines as u16,
            num_cols: self.columns as u16,
            cell_width,
            cell_height,
        }
    }
}

impl Dimensions for TerminalDimensions {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

fn terminal_config(scrolling_history: usize) -> Config {
    Config {
        scrolling_history,
        // 远端仍可通过用户主动选择复制；禁用 OSC 52，避免远端静默读写系统剪贴板。
        osc52: Osc52::Disabled,
        ..Config::default()
    }
}

fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut text = text.replace('\0', "");
    if !bracketed {
        return text.into_bytes();
    }
    // 删除嵌入的边界序列，防止粘贴内容提前结束 bracketed paste。
    text = text.replace("\u{1b}[200~", "").replace("\u{1b}[201~", "");
    format!("\u{1b}[200~{text}\u{1b}[201~").into_bytes()
}

fn bounded_text(mut text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

fn exit_status(status: ExitStatus) -> TerminalExit {
    TerminalExit {
        code: status.code(),
        success: status.success(),
    }
}

fn next_window_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests;
