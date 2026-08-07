//! Redis TTL 选择器。

use gpui::{
    App, ClickEvent, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, h_flex,
    input::{Input, InputState},
};

use crate::views::bounded_input;

const PRESETS: &[(&str, i64)] = &[
    ("5 分钟", 300),
    ("1 小时", 3600),
    ("1 天", 86_400),
    ("7 天", 604_800),
];
const MAX_TTL_INPUT_BYTES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Forever,
    Preset(usize),
    Custom,
}

pub struct TtlPicker {
    mode: Mode,
    custom: Entity<InputState>,
    disabled: bool,
}

impl TtlPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let custom =
            cx.new(|cx| bounded_input(MAX_TTL_INPUT_BYTES, window, cx).placeholder("自定义秒数"));
        Self {
            mode: Mode::Forever,
            custom,
            disabled: false,
        }
    }

    /// 将 PTTL 回填为永久、预设或自定义秒数。
    pub fn set_initial_ms(&mut self, ms: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let secs_opt = ttl_ms_to_secs(ms);
        match secs_opt {
            None => self.mode = Mode::Forever,
            Some(s) => {
                if let Some(idx) = PRESETS.iter().position(|(_, ps)| *ps == s) {
                    self.mode = Mode::Preset(idx);
                } else {
                    self.mode = Mode::Custom;
                    self.custom.update(cx, |state, cx_inner| {
                        state.set_value(s.to_string(), window, cx_inner);
                    });
                }
            }
        }
        cx.notify();
    }

    fn set_mode(&mut self, m: Mode, cx: &mut Context<Self>) {
        if !self.disabled && self.mode != m {
            self.mode = m;
            cx.notify();
        }
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            cx.notify();
        }
    }

    /// 永久返回 `None`，自定义输入非法时返回错误。
    pub fn collect(&self, cx: &App) -> Result<Option<i64>, String> {
        match self.mode {
            Mode::Forever => Ok(None),
            Mode::Preset(idx) => Ok(Some(PRESETS[idx].1)),
            Mode::Custom => {
                let raw = self.custom.read(cx).value().to_string();
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err("自定义 TTL 不能为空".into());
                }
                let secs: i64 = trimmed
                    .parse()
                    .map_err(|_| "自定义 TTL 必须是正整数".to_string())?;
                if secs <= 0 {
                    return Err("自定义 TTL 必须 > 0".into());
                }
                Ok(Some(secs))
            }
        }
    }
}

fn ttl_ms_to_secs(ms: Option<i64>) -> Option<i64> {
    match ms {
        // 向上取整，避免剩余不足一秒时回填为非法的 0，也不会因编辑而提前过期。
        Some(milliseconds) if milliseconds > 0 => Some(milliseconds.saturating_add(999) / 1000),
        _ => None,
    }
}

impl Render for TtlPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let fg = theme.foreground;
        let secondary_bg = theme.secondary;
        let border = theme.border;

        let mut accent_tint = accent;
        accent_tint.a = 0.10;
        let mut accent_border = accent;
        accent_border.a = 0.55;

        let chip =
            |label: String, id: &'static str, is_selected: bool| -> gpui::Stateful<gpui::Div> {
                let mut c = h_flex()
                    .id(SharedString::from(id))
                    .items_center()
                    .justify_center()
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded_md()
                    .border_1()
                    .text_xs()
                    .child(label);
                if is_selected {
                    c = c
                        .bg(accent_tint)
                        .border_color(accent_border)
                        .text_color(accent);
                } else {
                    c = c
                        .bg(secondary_bg)
                        .border_color(border)
                        .text_color(fg)
                        .hover(move |this| this.border_color(accent_border));
                }
                if self.disabled {
                    c = c.opacity(0.55);
                } else {
                    c = c.cursor_pointer();
                }
                c
            };

        let mut row = h_flex()
            .w_full()
            .items_center()
            .gap(px(8.0))
            .flex_wrap()
            .child(
                chip(
                    "永久".into(),
                    "ttl-forever",
                    matches!(self.mode, Mode::Forever),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.set_mode(Mode::Forever, cx);
                })),
            );

        for (i, (label, _secs)) in PRESETS.iter().enumerate() {
            let id_str: &'static str = match i {
                0 => "ttl-p0",
                1 => "ttl-p1",
                2 => "ttl-p2",
                3 => "ttl-p3",
                _ => "ttl-p?",
            };
            let is_selected = matches!(self.mode, Mode::Preset(idx) if idx == i);
            row = row.child(chip((*label).into(), id_str, is_selected).on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.set_mode(Mode::Preset(i), cx)),
            ));
        }

        let is_custom = matches!(self.mode, Mode::Custom);
        row =
            row.child(chip("自定义".into(), "ttl-custom", is_custom).on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.set_mode(Mode::Custom, cx)),
            ));

        if is_custom {
            row = row
                .child(
                    div()
                        .w(px(140.0))
                        .ml(px(4.0))
                        .child(Input::new(&self.custom).disabled(self.disabled)),
                )
                .child(div().text_xs().text_color(muted_fg).child("秒"));
        }

        row
    }
}

#[cfg(test)]
mod tests {
    use super::ttl_ms_to_secs;

    #[test]
    fn positive_subsecond_ttl_rounds_up_to_one_second() {
        assert_eq!(ttl_ms_to_secs(Some(1)), Some(1));
        assert_eq!(ttl_ms_to_secs(Some(999)), Some(1));
        assert_eq!(ttl_ms_to_secs(Some(1_001)), Some(2));
        assert_eq!(ttl_ms_to_secs(Some(-1)), None);
    }
}
