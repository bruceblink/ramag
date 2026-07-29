//! 普通 JSON 日志与 Shell 提示符的轻量着色；不改写 PTY 字节与 ANSI 状态。

mod prompt;

use gpui::{HighlightStyle, Hsla};
use gpui_component::Theme;

use crate::core::{RgbColor, TerminalCell};

#[derive(Clone, Copy)]
pub(super) struct SemanticPalette {
    property: RgbColor,
    string: RgbColor,
    escape: RgbColor,
    number: RgbColor,
    keyword: RgbColor,
    punctuation: RgbColor,
    info: RgbColor,
    warning: RgbColor,
    danger: RgbColor,
    user: RgbColor,
    host: RgbColor,
}

impl SemanticPalette {
    pub(super) fn plain(foreground: RgbColor) -> Self {
        Self {
            property: foreground,
            string: foreground,
            escape: foreground,
            number: foreground,
            keyword: foreground,
            punctuation: foreground,
            info: foreground,
            warning: foreground,
            danger: foreground,
            user: foreground,
            host: foreground,
        }
    }

    pub(super) fn from_theme(theme: &Theme) -> Self {
        let syntax = &theme.highlight_theme.style.syntax;
        Self {
            property: syntax_color(syntax.style("property"), theme.link),
            string: syntax_color(syntax.style("string"), theme.success),
            escape: syntax_color(syntax.style("string.escape"), theme.warning),
            number: syntax_color(syntax.style("number"), theme.warning),
            keyword: syntax_color(syntax.style("keyword"), theme.info),
            punctuation: syntax_color(syntax.style("punctuation"), theme.muted_foreground),
            info: super::rgb_color(theme.success),
            warning: super::rgb_color(theme.warning),
            danger: super::rgb_color(theme.danger),
            user: super::rgb_color(theme.link),
            host: super::rgb_color(theme.success),
        }
    }
}

pub(super) fn semantic_terminal_colors(
    rows: &[Vec<TerminalCell>],
    palette: SemanticPalette,
) -> Vec<Vec<Option<RgbColor>>> {
    let mut colors = rows
        .iter()
        .map(|row| vec![None; row.len()])
        .collect::<Vec<_>>();
    color_json(rows, &mut colors, palette);
    prompt::color_shell_prompts(
        rows,
        &mut colors,
        palette.user,
        palette.host,
        palette.punctuation,
    );
    colors
}

fn color_json(
    rows: &[Vec<TerminalCell>],
    colors: &mut [Vec<Option<RgbColor>>],
    palette: SemanticPalette,
) {
    let mut active = false;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut string_cells = Vec::new();
    let mut escape_cells = Vec::new();
    let mut string_sample = String::new();
    let mut sample_truncated = false;

    for (row_index, row) in rows.iter().enumerate() {
        if !active {
            let Some(start) = row.iter().position(|cell| !cell.text.trim().is_empty()) else {
                continue;
            };
            if !looks_like_json_start(rows, row_index, start) {
                continue;
            }
            active = true;
        }

        for column in 0..row.len() {
            let Some(character) = ascii_cell(row, column) else {
                if in_string {
                    string_cells.push((row_index, column));
                    sample_truncated = true;
                }
                continue;
            };
            if in_string {
                string_cells.push((row_index, column));
                if escaped {
                    escape_cells.push((row_index, column));
                    escaped = false;
                    continue;
                }
                match character {
                    b'\\' => {
                        escape_cells.push((row_index, column));
                        escaped = true;
                    }
                    b'"' => {
                        in_string = false;
                        let property = next_non_blank_ascii(rows, row_index, column) == Some(b':');
                        let color = if property {
                            palette.property
                        } else {
                            string_color(&string_sample, sample_truncated, palette)
                        };
                        apply_color(colors, &string_cells, color);
                        apply_color(colors, &escape_cells, palette.escape);
                        string_cells.clear();
                        escape_cells.clear();
                        string_sample.clear();
                        sample_truncated = false;
                    }
                    _ if !sample_truncated && string_sample.len() < 16 => {
                        string_sample.push(character as char);
                    }
                    _ => sample_truncated = true,
                }
                continue;
            }

            match character {
                b'"' => {
                    in_string = true;
                    string_cells.push((row_index, column));
                }
                b'{' | b'[' => {
                    depth = depth.saturating_add(1);
                    colors[row_index][column] = Some(palette.punctuation);
                }
                b'}' | b']' => {
                    depth = depth.saturating_sub(1);
                    colors[row_index][column] = Some(palette.punctuation);
                }
                b':' | b',' => colors[row_index][column] = Some(palette.punctuation),
                b'-' | b'0'..=b'9' => colors[row_index][column] = Some(palette.number),
                b'.' | b'e' | b'E' | b'+' if touches_number(row, column) => {
                    colors[row_index][column] = Some(palette.number);
                }
                b't' if word_at(row, column, b"true") => {
                    color_word(&mut colors[row_index], column, 4, palette.keyword);
                }
                b'f' if word_at(row, column, b"false") => {
                    color_word(&mut colors[row_index], column, 5, palette.keyword);
                }
                b'n' if word_at(row, column, b"null") => {
                    color_word(&mut colors[row_index], column, 4, palette.keyword);
                }
                _ => {}
            }
        }
        if active && !in_string && depth == 0 {
            active = false;
        }
    }
}

fn looks_like_json_start(rows: &[Vec<TerminalCell>], row: usize, column: usize) -> bool {
    let next = next_non_blank_ascii(rows, row, column);
    match ascii_cell(&rows[row], column) {
        Some(b'{') => matches!(next, Some(b'"' | b'}')),
        Some(b'[') => matches!(
            next,
            Some(b'{' | b'[' | b'"' | b']' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n')
        ),
        _ => false,
    }
}

fn string_color(value: &str, truncated: bool, palette: SemanticPalette) -> RgbColor {
    if truncated {
        return palette.string;
    }
    match value.to_ascii_lowercase().as_str() {
        "fatal" | "panic" | "error" => palette.danger,
        "warn" | "warning" => palette.warning,
        "info" | "success" => palette.info,
        "debug" | "trace" => palette.keyword,
        _ => palette.string,
    }
}

fn apply_color(colors: &mut [Vec<Option<RgbColor>>], cells: &[(usize, usize)], color: RgbColor) {
    for &(row, column) in cells {
        colors[row][column] = Some(color);
    }
}

fn color_word(colors: &mut [Option<RgbColor>], start: usize, length: usize, color: RgbColor) {
    for cell in colors.iter_mut().skip(start).take(length) {
        *cell = Some(color);
    }
}

fn word_at(row: &[TerminalCell], start: usize, expected: &[u8]) -> bool {
    expected
        .iter()
        .enumerate()
        .all(|(offset, byte)| ascii_cell(row, start + offset) == Some(*byte))
}

fn touches_number(row: &[TerminalCell], column: usize) -> bool {
    column
        .checked_sub(1)
        .and_then(|index| ascii_cell(row, index))
        .is_some_and(|character| character.is_ascii_digit())
        || ascii_cell(row, column + 1).is_some_and(|character| character.is_ascii_digit())
}

fn next_non_blank_ascii(rows: &[Vec<TerminalCell>], row: usize, column: usize) -> Option<u8> {
    rows.iter()
        .enumerate()
        .skip(row)
        .flat_map(|(candidate_row, cells)| {
            let start = if candidate_row == row { column + 1 } else { 0 };
            cells
                .iter()
                .enumerate()
                .skip(start)
                .map(move |(column, _)| ascii_cell(cells, column))
        })
        .flatten()
        .find(|character| !character.is_ascii_whitespace())
}

fn ascii_cell(row: &[TerminalCell], column: usize) -> Option<u8> {
    let text = row.get(column)?.text.as_bytes();
    (text.len() == 1 && text[0].is_ascii()).then_some(text[0])
}

fn syntax_color(style: Option<HighlightStyle>, fallback: Hsla) -> RgbColor {
    super::rgb_color(style.and_then(|style| style.color).unwrap_or(fallback))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::TerminalStyle;

    const PROPERTY: RgbColor = color(1);
    const STRING: RgbColor = color(2);
    const ESCAPE: RgbColor = color(3);
    const NUMBER: RgbColor = color(4);
    const KEYWORD: RgbColor = color(5);
    const PUNCTUATION: RgbColor = color(6);
    const INFO: RgbColor = color(7);
    const WARNING: RgbColor = color(8);
    const DANGER: RgbColor = color(9);

    #[test]
    fn json_log_uses_semantic_colors() {
        let text = r#"{"level":"error","count":5,"ok":false}"#;
        let rows = vec![row(text)];
        let colors = semantic_terminal_colors(&rows, palette());

        assert_eq!(colors[0][text.find("level").unwrap()], Some(PROPERTY));
        assert_eq!(colors[0][text.find("error").unwrap()], Some(DANGER));
        assert_eq!(colors[0][text.find('5').unwrap()], Some(NUMBER));
        assert_eq!(colors[0][text.find("false").unwrap()], Some(KEYWORD));
        assert_eq!(colors[0][0], Some(PUNCTUATION));
    }

    #[test]
    fn wrapped_json_string_keeps_state_and_colors_escape() {
        let rows = vec![row(r#"{"msg":"line\"#), row(r#"nnext","level":"info"}"#)];
        let colors = semantic_terminal_colors(&rows, palette());

        assert_eq!(colors[1][0], Some(ESCAPE));
        let info = rows[1].iter().position(|cell| cell.text == "i").unwrap();
        assert_eq!(colors[1][info], Some(INFO));
    }

    #[test]
    fn plain_terminal_output_is_not_recolored() {
        for text in ["echo 500", "mail user@example.com"] {
            let colors = semantic_terminal_colors(&[row(text)], palette());
            assert!(colors[0].iter().all(Option::is_none), "{text}");
        }
    }

    fn row(text: &str) -> Vec<TerminalCell> {
        text.chars()
            .map(|character| TerminalCell {
                text: character.to_string(),
                foreground: color(0),
                background: color(0),
                style: TerminalStyle::default(),
                selected: false,
                wide_spacer: false,
            })
            .collect()
    }

    fn palette() -> SemanticPalette {
        SemanticPalette {
            property: PROPERTY,
            string: STRING,
            escape: ESCAPE,
            number: NUMBER,
            keyword: KEYWORD,
            punctuation: PUNCTUATION,
            info: INFO,
            warning: WARNING,
            danger: DANGER,
            user: color(10),
            host: color(11),
        }
    }

    const fn color(red: u8) -> RgbColor {
        RgbColor {
            red,
            green: 0,
            blue: 0,
        }
    }
}
