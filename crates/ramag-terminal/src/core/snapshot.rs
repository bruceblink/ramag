use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode, point_to_viewport};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use super::{
    RgbColor, TerminalCell, TerminalCursor, TerminalCursorShape, TerminalEventProxy,
    TerminalSnapshot, TerminalStyle,
};

pub(super) fn snapshot_term(terminal: &Term<TerminalEventProxy>) -> TerminalSnapshot {
    let content = terminal.renderable_content();
    let columns = terminal.columns();
    let lines = terminal.screen_lines();
    let mut rows = vec![Vec::with_capacity(columns); lines];
    for indexed in content.display_iter {
        let Some(viewport) = point_to_viewport(content.display_offset, indexed.point) else {
            continue;
        };
        if viewport.line >= lines {
            continue;
        }
        let selected = content
            .selection
            .is_some_and(|selection| selection.contains(indexed.point));
        let cell = indexed.cell;
        let inverse = cell.flags.contains(Flags::INVERSE);
        let (foreground, background) = if inverse {
            (
                resolve_color(cell.bg, content.colors, false),
                resolve_color(cell.fg, content.colors, true),
            )
        } else {
            (
                resolve_color(cell.fg, content.colors, true),
                resolve_color(cell.bg, content.colors, false),
            )
        };
        let text = if cell.flags.contains(Flags::HIDDEN) {
            " ".into()
        } else {
            let mut text = cell.c.to_string();
            if let Some(characters) = cell.zerowidth() {
                text.extend(characters);
            }
            text
        };
        rows[viewport.line].push(TerminalCell {
            text,
            foreground,
            background,
            style: TerminalStyle {
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                strikeout: cell.flags.contains(Flags::STRIKEOUT),
                dim: cell.flags.contains(Flags::DIM),
            },
            selected,
            wide_spacer: cell.flags.contains(Flags::WIDE_CHAR_SPACER),
        });
    }
    for row in &mut rows {
        row.resize_with(columns, || TerminalCell {
            text: " ".into(),
            foreground: default_foreground(),
            background: default_background(),
            style: TerminalStyle::default(),
            selected: false,
            wide_spacer: false,
        });
    }
    let cursor =
        point_to_viewport(content.display_offset, content.cursor.point).and_then(|point| {
            (point.line < lines).then_some(TerminalCursor {
                row: point.line,
                column: point.column.0,
                shape: match content.cursor.shape {
                    alacritty_terminal::vte::ansi::CursorShape::Hidden => {
                        TerminalCursorShape::Hidden
                    }
                    alacritty_terminal::vte::ansi::CursorShape::Block => TerminalCursorShape::Block,
                    alacritty_terminal::vte::ansi::CursorShape::Underline => {
                        TerminalCursorShape::Underline
                    }
                    alacritty_terminal::vte::ansi::CursorShape::Beam => TerminalCursorShape::Beam,
                    alacritty_terminal::vte::ansi::CursorShape::HollowBlock => {
                        TerminalCursorShape::HollowBlock
                    }
                },
            })
        });
    TerminalSnapshot {
        columns,
        lines,
        rows,
        cursor,
        alternate_screen: content.mode.contains(TermMode::ALT_SCREEN),
        bracketed_paste: content.mode.contains(TermMode::BRACKETED_PASTE),
        application_cursor: content.mode.contains(TermMode::APP_CURSOR),
        display_offset: content.display_offset,
    }
}

pub(super) fn viewport_point(
    terminal: &Term<TerminalEventProxy>,
    row: usize,
    column: usize,
) -> Point {
    let row = row.min(terminal.screen_lines().saturating_sub(1));
    let column = column.min(terminal.columns().saturating_sub(1));
    Point::new(
        Line(row as i32 - terminal.grid().display_offset() as i32),
        Column(column),
    )
}

fn resolve_color(
    color: Color,
    overrides: &alacritty_terminal::term::color::Colors,
    foreground: bool,
) -> RgbColor {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(index) => {
            overrides[index as usize].unwrap_or_else(|| indexed_color(index as usize))
        }
        Color::Named(named) => overrides[named].unwrap_or_else(|| named_color(named, foreground)),
    };
    RgbColor::new(rgb.r, rgb.g, rgb.b)
}

pub(super) fn indexed_color(index: usize) -> Rgb {
    if index < 16 {
        return named_palette(index);
    }
    if index < 232 {
        let value = index - 16;
        let red = value / 36;
        let green = (value / 6) % 6;
        let blue = value % 6;
        let component = |part: usize| if part == 0 { 0 } else { 55 + part as u8 * 40 };
        return Rgb {
            r: component(red),
            g: component(green),
            b: component(blue),
        };
    }
    let grey = 8u8.saturating_add((index.saturating_sub(232).min(23) as u8) * 10);
    Rgb {
        r: grey,
        g: grey,
        b: grey,
    }
}

fn named_color(named: NamedColor, foreground: bool) -> Rgb {
    match named {
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            let color = default_foreground();
            Rgb {
                r: color.red,
                g: color.green,
                b: color.blue,
            }
        }
        NamedColor::Background | NamedColor::Cursor => {
            let color = if foreground {
                default_foreground()
            } else {
                default_background()
            };
            Rgb {
                r: color.red,
                g: color.green,
                b: color.blue,
            }
        }
        _ => named_palette(named as usize),
    }
}

fn named_palette(index: usize) -> Rgb {
    const PALETTE: [[u8; 3]; 16] = [
        [0x00, 0x00, 0x00],
        [0xcd, 0x31, 0x31],
        [0x0d, 0xbc, 0x79],
        [0xe5, 0xe5, 0x10],
        [0x24, 0x72, 0xc8],
        [0xbc, 0x3f, 0xbc],
        [0x11, 0xa8, 0xcd],
        [0xe5, 0xe5, 0xe5],
        [0x66, 0x66, 0x66],
        [0xf1, 0x4c, 0x4c],
        [0x23, 0xd1, 0x8b],
        [0xf5, 0xf5, 0x43],
        [0x3b, 0x8e, 0xf3],
        [0xd6, 0x70, 0xd6],
        [0x29, 0xb8, 0xdb],
        [0xff, 0xff, 0xff],
    ];
    let [r, g, b] = PALETTE[index.min(15)];
    Rgb { r, g, b }
}

pub(super) fn default_foreground() -> RgbColor {
    RgbColor::new(0xd4, 0xd4, 0xd4)
}

fn default_background() -> RgbColor {
    RgbColor::new(0x1e, 0x1e, 0x1e)
}
