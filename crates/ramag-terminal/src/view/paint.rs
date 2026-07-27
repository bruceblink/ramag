//! 终端快照到 GPUI 低层绘制命令的转换。

use gpui::{
    App, Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, SharedString, StrikethroughStyle,
    TextAlign, TextRun, UnderlineStyle, Window, fill, font, point, px, rgb,
};

use crate::core::{RgbColor, TerminalCell, TerminalCursorShape, TerminalSnapshot, TerminalStyle};

use super::{FONT_SIZE, LINE_HEIGHT};

pub(super) struct PreparedTerminal {
    snapshot: TerminalSnapshot,
    marked_text: String,
    cell_width: Pixels,
    mono: SharedString,
    background: RgbColor,
}

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
    _window: &mut Window,
) -> PreparedTerminal {
    let background = snapshot
        .rows
        .first()
        .and_then(|row| row.first())
        .map(|cell| cell.background)
        .unwrap_or(RgbColor {
            red: 0x1e,
            green: 0x1e,
            blue: 0x1e,
        });
    PreparedTerminal {
        snapshot,
        marked_text,
        cell_width,
        mono,
        background,
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
        paint_text(row, row_index, &prepared, bounds, window, cx);
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
        let background = row[start].background;
        let mut end = start + 1;
        while end < row.len() && row[end].selected == selected && row[end].background == background
        {
            end += 1;
        }
        if selected || background != prepared.background {
            let paint = if selected {
                rgb(0x264f78).into()
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
        let foreground = if row[start].selected {
            RgbColor {
                red: 0xff,
                green: 0xff,
                blue: 0xff,
            }
        } else {
            row[start].foreground
        };
        let mut text = String::new();
        let mut end = start;
        while end < row.len()
            && !row[end].wide_spacer
            && row[end].style == style
            && row[end].foreground == row[start].foreground
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
        tracing::warn!(error = %error, "paint terminal text failed");
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
        gpui::rgba(0xaaaaaa59)
    } else {
        gpui::rgba(0xaaaaaabf)
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
        RgbColor {
            red: 0xff,
            green: 0xff,
            blue: 0xff,
        },
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
