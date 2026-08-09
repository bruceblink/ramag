//! 终端快照到 GPUI 低层绘制命令的转换。

mod semantic;

use gpui::{
    App, Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, Rgba, SharedString, StrikethroughStyle,
    TextAlign, TextRun, UnderlineStyle, Window, fill, font, point, px, rgb,
};
use gpui_component::Theme;

use crate::core::{RgbColor, TerminalCell, TerminalCursorShape, TerminalSnapshot, TerminalStyle};

use super::{FONT_SIZE, LINE_HEIGHT};
use semantic::{SemanticPalette, semantic_terminal_colors};

pub(super) struct PreparedTerminal {
    snapshot: TerminalSnapshot,
    semantic_colors: Vec<Vec<Option<RgbColor>>>,
    marked_text: String,
    cell_width: Pixels,
    mono: SharedString,
    background: RgbColor,
    palette: TerminalPalette,
}

#[derive(Clone, Copy)]
pub(super) struct TerminalPalette {
    background: RgbColor,
    foreground: RgbColor,
    selection_background: RgbColor,
    selection_foreground: RgbColor,
    cursor: RgbColor,
    semantic: SemanticPalette,
}

impl TerminalPalette {
    pub(super) fn from_theme(
        background: Hsla,
        foreground: Hsla,
        selection_background: Hsla,
        selection_foreground: Hsla,
        cursor: Hsla,
    ) -> Self {
        let foreground = rgb_color(foreground);
        Self {
            background: rgb_color(background),
            foreground,
            selection_background: rgb_color(selection_background),
            selection_foreground: rgb_color(selection_foreground),
            cursor: rgb_color(cursor),
            semantic: SemanticPalette::plain(foreground),
        }
    }

    pub(super) fn from_component_theme(theme: &Theme) -> Self {
        let mut palette = Self::from_theme(
            theme.background,
            theme.foreground,
            theme.accent,
            theme.accent_foreground,
            theme.caret,
        );
        palette.semantic = SemanticPalette::from_theme(theme);
        palette
    }
}

const DARK_BACKGROUND: RgbColor = RgbColor {
    red: 0x1e,
    green: 0x1e,
    blue: 0x1e,
};
const DARK_FOREGROUND: RgbColor = RgbColor {
    red: 0xd4,
    green: 0xd4,
    blue: 0xd4,
};

pub(super) fn measure_cell_width(
    window: &mut Window,
    mono: &SharedString,
    font_size: Pixels,
) -> Pixels {
    let run = TextRun {
        len: 1,
        font: font(mono.clone()),
        color: rgb(0xffffff).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line("M".into(), font_size, &[run], None)
        .width()
        .max(px(1.0))
}

pub(super) fn prepare(
    snapshot: TerminalSnapshot,
    marked_text: String,
    cell_width: Pixels,
    mono: SharedString,
    palette: TerminalPalette,
    _window: &mut Window,
) -> PreparedTerminal {
    let semantic_colors = semantic_terminal_colors(&snapshot.rows, palette.semantic);
    PreparedTerminal {
        snapshot,
        semantic_colors,
        marked_text,
        cell_width,
        mono,
        background: palette.background,
        palette,
    }
}

pub(super) fn paint(
    prepared: PreparedTerminal,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(fill(bounds, color(prepared.background)));

    for (row_index, row) in prepared.snapshot.rows.iter().enumerate() {
        paint_backgrounds(row, row_index, &prepared, bounds, window);
        paint_text(
            row,
            &prepared.semantic_colors[row_index],
            row_index,
            &prepared,
            bounds,
            window,
            cx,
        );
    }
    paint_cursor(&prepared, bounds, window);
    paint_marked_text(&prepared, bounds, window, cx);
}

fn paint_backgrounds(
    row: &[TerminalCell],
    row_index: usize,
    prepared: &PreparedTerminal,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    let mut start = 0usize;
    while start < row.len() {
        let selected = row[start].selected;
        let background = themed_color(row[start].background, prepared.palette);
        let mut end = start + 1;
        while end < row.len()
            && row[end].selected == selected
            && themed_color(row[end].background, prepared.palette) == background
        {
            end += 1;
        }
        if selected || background != prepared.background {
            let paint = if selected {
                color(prepared.palette.selection_background)
            } else {
                color(background)
            };
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        bounds.left() + prepared.cell_width * start as f32,
                        bounds.top() + LINE_HEIGHT * row_index as f32,
                    ),
                    gpui::size(prepared.cell_width * (end - start) as f32, LINE_HEIGHT),
                ),
                paint,
            ));
        }
        start = end;
    }
}

fn paint_text(
    row: &[TerminalCell],
    semantic_colors: &[Option<RgbColor>],
    row_index: usize,
    prepared: &PreparedTerminal,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut start = 0usize;
    while start < row.len() {
        if row[start].wide_spacer || row[start].text == " " {
            start += 1;
            continue;
        }
        let style = row[start].style;
        let foreground = cell_foreground(&row[start], semantic_colors[start], prepared.palette);
        let mut text = String::new();
        let mut end = start;
        while end < row.len()
            && !row[end].wide_spacer
            && row[end].style == style
            && cell_foreground(&row[end], semantic_colors[end], prepared.palette) == foreground
            && row[end].selected == row[start].selected
        {
            text.push_str(&row[end].text);
            end += 1;
        }
        let text = text.trim_end_matches(' ');
        if !text.is_empty() {
            paint_fragment(
                text,
                style,
                foreground,
                point(
                    bounds.left() + prepared.cell_width * start as f32,
                    bounds.top() + LINE_HEIGHT * row_index as f32,
                ),
                &prepared.mono,
                window,
                cx,
            );
        }
        start = end.max(start + 1);
    }
}

fn cell_foreground(
    cell: &TerminalCell,
    semantic: Option<RgbColor>,
    palette: TerminalPalette,
) -> RgbColor {
    if cell.selected {
        palette.selection_foreground
    } else if cell.foreground == DARK_FOREGROUND {
        semantic.unwrap_or_else(|| themed_color(cell.foreground, palette))
    } else {
        themed_color(cell.foreground, palette)
    }
}

fn paint_fragment(
    text: &str,
    style: TerminalStyle,
    foreground: RgbColor,
    origin: gpui::Point<Pixels>,
    mono: &SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    let mut terminal_font: Font = font(mono.clone());
    terminal_font.weight = if style.bold {
        FontWeight::BOLD
    } else {
        FontWeight::NORMAL
    };
    terminal_font.style = if style.italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    let foreground = color(foreground);
    let run = TextRun {
        len: text.len(),
        font: terminal_font,
        color: if style.dim {
            foreground.opacity(0.66)
        } else {
            foreground
        },
        background_color: None,
        underline: style.underline.then_some(UnderlineStyle {
            color: Some(foreground),
            thickness: px(1.0),
            wavy: false,
        }),
        strikethrough: style.strikeout.then_some(StrikethroughStyle {
            color: Some(foreground),
            thickness: px(1.0),
        }),
    };
    let line = window
        .text_system()
        .shape_line(text.to_string().into(), FONT_SIZE, &[run], None);
    if let Err(error) = line.paint(origin, LINE_HEIGHT, TextAlign::Left, None, window, cx) {
        tracing::warn!(
            operation = "terminal_text_paint",
            error = %error,
            "paint terminal text failed"
        );
    }
}

fn paint_cursor(prepared: &PreparedTerminal, bounds: Bounds<Pixels>, window: &mut Window) {
    let Some(cursor) = prepared.snapshot.cursor else {
        return;
    };
    if cursor.shape == TerminalCursorShape::Hidden {
        return;
    }
    let origin = point(
        bounds.left() + prepared.cell_width * cursor.column as f32,
        bounds.top() + LINE_HEIGHT * cursor.row as f32,
    );
    let cursor_bounds = match cursor.shape {
        TerminalCursorShape::Block | TerminalCursorShape::HollowBlock => {
            Bounds::new(origin, gpui::size(prepared.cell_width, LINE_HEIGHT))
        }
        TerminalCursorShape::Underline => Bounds::new(
            point(origin.x, origin.y + LINE_HEIGHT - px(2.0)),
            gpui::size(prepared.cell_width, px(2.0)),
        ),
        TerminalCursorShape::Beam => Bounds::new(origin, gpui::size(px(2.0), LINE_HEIGHT)),
        TerminalCursorShape::Hidden => return,
    };
    let color = if cursor.shape == TerminalCursorShape::HollowBlock {
        color(prepared.palette.cursor).opacity(0.35)
    } else {
        color(prepared.palette.cursor).opacity(0.75)
    };
    window.paint_quad(fill(cursor_bounds, color));
}

fn paint_marked_text(
    prepared: &PreparedTerminal,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(cursor) = prepared.snapshot.cursor else {
        return;
    };
    if prepared.marked_text.is_empty() {
        return;
    }
    paint_fragment(
        &prepared.marked_text,
        TerminalStyle {
            underline: true,
            ..Default::default()
        },
        prepared.palette.foreground,
        point(
            bounds.left() + prepared.cell_width * cursor.column as f32,
            bounds.top() + LINE_HEIGHT * cursor.row as f32,
        ),
        &prepared.mono,
        window,
        cx,
    );
}

fn color(value: RgbColor) -> Hsla {
    rgb((u32::from(value.red) << 16) | (u32::from(value.green) << 8) | u32::from(value.blue)).into()
}

fn rgb_color(value: Hsla) -> RgbColor {
    let value = Rgba::from(value);
    RgbColor {
        red: (value.r * 255.0).round() as u8,
        green: (value.g * 255.0).round() as u8,
        blue: (value.b * 255.0).round() as u8,
    }
}

fn themed_color(value: RgbColor, palette: TerminalPalette) -> RgbColor {
    if value == DARK_BACKGROUND {
        palette.background
    } else if value == DARK_FOREGROUND {
        palette.foreground
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_palette_replaces_default_terminal_colors() {
        let palette = TerminalPalette::from_theme(
            rgb(0xf8f8f8).into(),
            rgb(0x242424).into(),
            rgb(0xadd6ff).into(),
            rgb(0x1f2328).into(),
            rgb(0x555555).into(),
        );

        assert_eq!(themed_color(DARK_BACKGROUND, palette), palette.background);
        assert_eq!(themed_color(DARK_FOREGROUND, palette), palette.foreground);
    }

    #[test]
    fn server_ansi_color_takes_priority_over_semantic_log_color() {
        let palette = TerminalPalette::from_theme(
            rgb(0xf8f8f8).into(),
            rgb(0x242424).into(),
            rgb(0xadd6ff).into(),
            rgb(0x1f2328).into(),
            rgb(0x555555).into(),
        );
        let ansi = RgbColor {
            red: 12,
            green: 34,
            blue: 56,
        };
        let semantic = RgbColor {
            red: 200,
            green: 0,
            blue: 0,
        };
        let cell = TerminalCell {
            text: "x".into(),
            foreground: ansi,
            background: DARK_BACKGROUND,
            style: TerminalStyle::default(),
            selected: false,
            wide_spacer: false,
        };

        assert_eq!(cell_foreground(&cell, Some(semantic), palette), ansi);
    }
}
