//! GPUI 终端视图。

mod input;
mod paint;

use std::ops::Range;
use std::time::Duration;

use alacritty_terminal::index::Side;
use gpui::{
    Bounds, ClipboardItem, Context, ElementInputHandler, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollWheelEvent, Window, canvas, div, prelude::*, px,
};
use gpui_component::ActiveTheme as _;

use crate::core::{ClipboardRequest, TerminalCore};
use crate::keys::{TerminalKey, TerminalModifiers, encode_key};
use crate::{SendBackTab, SendTab};

const FONT_SIZE: Pixels = px(13.0);
const LINE_HEIGHT: Pixels = px(18.0);

pub struct TerminalView {
    core: TerminalCore,
    focus_handle: FocusHandle,
    last_revision: u64,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    selecting: bool,
    marked_text: String,
    marked_selection_utf16: Range<usize>,
}

impl TerminalView {
    pub fn new(core: TerminalCore, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let last_revision = core.revision();
        cx.spawn_in(window, async move |this, async_cx| {
            loop {
                async_cx
                    .background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if this
                    .update_in(async_cx, |this, _window, cx| {
                        let revision = this.core.revision();
                        if revision != this.last_revision {
                            this.last_revision = revision;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            core,
            focus_handle: cx.focus_handle(),
            last_revision,
            bounds: Bounds::default(),
            cell_width: px(8.0),
            selecting: false,
            marked_text: String::new(),
            marked_selection_utf16: 0..0,
        }
    }

    pub fn title(&self) -> Option<String> {
        self.core.title()
    }

    pub fn core(&self) -> &TerminalCore {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut TerminalCore {
        &mut self.core
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        // AltGr 等组合键应交给系统文本输入，避免误判成 Ctrl+Alt 快捷键。
        if event.prefer_character_input {
            return;
        }
        let modifiers = terminal_modifiers(&event.keystroke.modifiers);
        let key_name = event.keystroke.key.as_str();
        if is_copy_shortcut(key_name, modifiers) {
            self.copy_selection(cx);
            cx.stop_propagation();
            return;
        }
        if is_paste_shortcut(key_name, modifiers) {
            self.paste_clipboard(cx);
            cx.stop_propagation();
            return;
        }
        let key = match key_name {
            "enter" => Some(TerminalKey::Enter),
            "backspace" => Some(TerminalKey::Backspace),
            "tab" => Some(TerminalKey::Tab),
            "escape" => Some(TerminalKey::Escape),
            "up" => Some(TerminalKey::Up),
            "down" => Some(TerminalKey::Down),
            "left" => Some(TerminalKey::Left),
            "right" => Some(TerminalKey::Right),
            "home" => Some(TerminalKey::Home),
            "end" => Some(TerminalKey::End),
            "insert" => Some(TerminalKey::Insert),
            "delete" => Some(TerminalKey::Delete),
            "pageup" => Some(TerminalKey::PageUp),
            "pagedown" => Some(TerminalKey::PageDown),
            key if key.len() >= 2 && key.starts_with('f') => {
                key[1..].parse::<u8>().ok().map(TerminalKey::Function)
            }
            _ if modifiers.control || modifiers.alt => event
                .keystroke
                .key_char
                .clone()
                .or_else(|| Some(event.keystroke.key.clone()))
                .map(TerminalKey::Character),
            _ => None,
        };
        let Some(key) = key else {
            return;
        };
        self.send_key(key, modifiers, cx);
    }

    fn send_key(&mut self, key: TerminalKey, modifiers: TerminalModifiers, cx: &mut Context<Self>) {
        let mode = self.core.snapshot();
        let mut term_mode = alacritty_terminal::term::TermMode::empty();
        if mode.bracketed_paste {
            term_mode.insert(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
        }
        if mode.application_cursor {
            term_mode.insert(alacritty_terminal::term::TermMode::APP_CURSOR);
        }
        if let Some(bytes) = encode_key(&key, modifiers, term_mode) {
            self.core.scroll_to_bottom();
            if let Err(error) = self.core.send(bytes) {
                tracing::warn!(
                    operation = "terminal_key_send",
                    error = %error,
                    "send terminal key failed"
                );
            }
            cx.stop_propagation();
        }
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.core.selected_text()
            && !text.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_clipboard(&self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if let Err(error) = self.core.paste(&text) {
            tracing::warn!(
                operation = "terminal_clipboard_paste",
                bytes = text.len(),
                error = %error,
                "paste terminal clipboard failed"
            );
        }
    }

    fn process_clipboard_requests(&self, cx: &mut Context<Self>) {
        for request in self.core.take_clipboard_requests() {
            match request {
                ClipboardRequest::Store(text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                ClipboardRequest::Load(formatter) => {
                    let text = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .unwrap_or_default();
                    let response = formatter(&text);
                    if let Err(error) = self.core.send(response.into_bytes()) {
                        tracing::warn!(
                            operation = "terminal_clipboard_reply",
                            clipboard_bytes = text.len(),
                            error = %error,
                            "reply terminal clipboard request failed"
                        );
                    }
                }
            }
        }
    }

    fn grid_point(&self, point: Point<Pixels>) -> (usize, usize, Side) {
        let relative_x = (point.x - self.bounds.left()).max(Pixels::ZERO);
        let relative_y = (point.y - self.bounds.top()).max(Pixels::ZERO);
        let column_float = relative_x / self.cell_width.max(px(1.0));
        let column = column_float.floor().max(0.0) as usize;
        let row = (relative_y / LINE_HEIGHT).floor().max(0.0) as usize;
        let side = if column_float - column as f32 > 0.5 {
            Side::Right
        } else {
            Side::Left
        };
        (row, column, side)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.button != MouseButton::Left {
            return;
        }
        self.focus_handle.focus(window, cx);
        let (row, column, side) = self.grid_point(event.position);
        self.core.start_selection(row, column, side);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        let (row, column, side) = self.grid_point(event.position);
        self.core.update_selection(row, column, side);
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.selecting = false;
            cx.notify();
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(window.line_height()).y;
        if delta == Pixels::ZERO {
            return;
        }
        self.core.scroll((delta / LINE_HEIGHT).round() as i32);
        cx.stop_propagation();
        cx.notify();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.process_clipboard_requests(cx);
        let entity_for_prepaint = cx.entity().clone();
        let entity_for_paint = cx.entity().clone();
        let focus_handle = self.focus_handle.clone();
        let mono = cx.theme().mono_font_family.clone();
        let terminal_background = cx.theme().background;
        let palette = paint::TerminalPalette::from_component_theme(cx.theme());
        let terminal = canvas(
            move |bounds, window, app| {
                entity_for_prepaint.update(app, |view, _cx| {
                    let cell_width = paint::measure_cell_width(window, &mono, FONT_SIZE);
                    view.bounds = bounds;
                    view.cell_width = cell_width;
                    let columns = (bounds.size.width / cell_width.max(px(1.0))).floor() as usize;
                    let lines = (bounds.size.height / LINE_HEIGHT).floor() as usize;
                    let columns = columns.max(2);
                    let lines = lines.max(1);
                    let cell_width_px =
                        cell_width.as_f32().ceil().clamp(1.0, u16::MAX as f32) as u16;
                    let line_height_px = LINE_HEIGHT.as_f32().ceil() as u16;
                    if let Err(error) =
                        view.core
                            .resize(columns, lines, cell_width_px, line_height_px)
                    {
                        tracing::warn!(
                            operation = "terminal_resize",
                            columns,
                            lines,
                            cell_width_px,
                            line_height_px,
                            error = %error,
                            "resize terminal view failed"
                        );
                    }
                    paint::prepare(
                        view.core.snapshot(),
                        view.marked_text.clone(),
                        cell_width,
                        mono.clone(),
                        palette,
                        window,
                    )
                })
            },
            move |bounds, prepared, window, app| {
                window.handle_input(
                    &focus_handle,
                    ElementInputHandler::new(bounds, entity_for_paint),
                    app,
                );
                paint::paint(prepared, bounds, window, app);
            },
        )
        .size_full();

        div()
            .id("ramag-terminal-view")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .px(px(10.0))
            .py(px(8.0))
            .bg(terminal_background)
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &SendTab, _window, cx| {
                this.send_key(TerminalKey::Tab, TerminalModifiers::default(), cx);
            }))
            .on_action(cx.listener(|this, _: &SendBackTab, _window, cx| {
                this.send_key(
                    TerminalKey::Tab,
                    TerminalModifiers {
                        shift: true,
                        ..Default::default()
                    },
                    cx,
                );
            }))
            .on_key_down(cx.listener(|this, event, _window, cx| this.handle_key(event, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(|this, event, _window, cx| this.mouse_move(event, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event, _window, cx| {
                    this.mouse_up(event, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(terminal)
    }
}

fn terminal_modifiers(modifiers: &gpui::Modifiers) -> TerminalModifiers {
    TerminalModifiers {
        control: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        platform: modifiers.platform,
    }
}

#[cfg(target_os = "macos")]
fn is_copy_shortcut(key: &str, modifiers: TerminalModifiers) -> bool {
    key.eq_ignore_ascii_case("c") && modifiers.platform && !modifiers.control && !modifiers.alt
}

#[cfg(not(target_os = "macos"))]
fn is_copy_shortcut(key: &str, modifiers: TerminalModifiers) -> bool {
    key.eq_ignore_ascii_case("c") && modifiers.control && modifiers.shift && !modifiers.alt
}

#[cfg(target_os = "macos")]
fn is_paste_shortcut(key: &str, modifiers: TerminalModifiers) -> bool {
    key.eq_ignore_ascii_case("v") && modifiers.platform && !modifiers.control && !modifiers.alt
}

#[cfg(not(target_os = "macos"))]
fn is_paste_shortcut(key: &str, modifiers: TerminalModifiers) -> bool {
    key.eq_ignore_ascii_case("v") && modifiers.control && modifiers.shift && !modifiers.alt
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    use super::*;
    #[cfg(unix)]
    use crate::TerminalCommand;

    #[cfg(unix)]
    struct TerminalKeyTestRoot {
        terminal: gpui::Entity<TerminalView>,
    }

    #[cfg(unix)]
    impl Render for TerminalKeyTestRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(self.terminal.clone())
                .child(gpui_component::button::Button::new("next-focus-target").label("下一项"))
        }
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_stays_in_terminal_and_reaches_pty(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::init(cx);
        });
        let mut terminal = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let core = TerminalCore::start(TerminalCommand::new(
                "/bin/sh",
                vec![
                    "-c".into(),
                    "stty -echo -icanon min 1 time 0; od -An -t u1 -N 1".into(),
                ],
            ))
            .expect("测试终端应启动");
            let terminal_view = cx.new(|cx| TerminalView::new(core, window, cx));
            terminal_view.read(cx).focus_handle(cx).focus(window, cx);
            terminal = Some(terminal_view.clone());
            let content = cx.new(|_| TerminalKeyTestRoot {
                terminal: terminal_view,
            });
            gpui_component::Root::new(content, window, cx)
        });
        let terminal = terminal.expect("终端视图应创建");

        assert!(
            cx.update(|window, app| { terminal.read(app).focus_handle(app).is_focused(window) })
        );
        cx.simulate_keystrokes("tab");
        assert!(
            cx.update(|window, app| { terminal.read(app).focus_handle(app).is_focused(window) })
        );

        let deadline = Instant::now() + Duration::from_secs(3);
        let received = loop {
            let output = terminal.read_with(cx, |terminal, _| {
                terminal
                    .core()
                    .snapshot()
                    .rows
                    .iter()
                    .flat_map(|row| row.iter().map(|cell| cell.text.as_str()))
                    .collect::<String>()
            });
            if output.split_whitespace().any(|value| value == "9") {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(received, "Tab 应作为字节 9 写入 PTY");
    }
}
