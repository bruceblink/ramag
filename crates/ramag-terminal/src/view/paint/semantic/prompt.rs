//! 常见 Shell 提示符中的用户名与主机名着色。

use crate::core::{RgbColor, TerminalCell};

use super::ascii_cell;

pub(super) fn color_shell_prompts(
    rows: &[Vec<TerminalCell>],
    colors: &mut [Vec<Option<RgbColor>>],
    user_color: RgbColor,
    host_color: RgbColor,
    punctuation_color: RgbColor,
) {
    for (row_index, row) in rows.iter().enumerate() {
        let Some((user_start, at, host_end)) = prompt_identity(row) else {
            continue;
        };
        color_range(&mut colors[row_index], user_start, at, user_color);
        colors[row_index][at] = Some(punctuation_color);
        color_range(&mut colors[row_index], at + 1, host_end, host_color);
    }
}

fn prompt_identity(row: &[TerminalCell]) -> Option<(usize, usize, usize)> {
    let first = row.iter().position(|cell| !cell.text.trim().is_empty())?;
    for at in first..row.len() {
        if ascii_cell(row, at) != Some(b'@') {
            continue;
        }
        let user_start = identifier_start(row, at);
        let host_end = identifier_end(row, at + 1);
        if user_start == at || host_end == at + 1 || !valid_prompt_prefix(row, first, user_start) {
            continue;
        }
        if has_shell_marker(row, first, host_end) {
            return Some((user_start, at, host_end));
        }
    }
    None
}

fn valid_prompt_prefix(row: &[TerminalCell], first: usize, user_start: usize) -> bool {
    user_start == first
        || (ascii_cell(row, first) == Some(b'[') && user_start == first.saturating_add(1))
}

fn has_shell_marker(row: &[TerminalCell], first: usize, host_end: usize) -> bool {
    if ascii_cell(row, first) == Some(b'[') {
        let close = (host_end..row.len()).find(|index| ascii_cell(row, *index) == Some(b']'));
        return close
            .and_then(|index| next_non_blank(row, index + 1))
            .is_some_and(|index| is_shell_marker(ascii_cell(row, index)));
    }
    if has_windows_cmd_prompt(row, host_end) {
        return true;
    }
    for (index, _) in row.iter().enumerate().skip(host_end).take(256) {
        let character = ascii_cell(row, index);
        if character.is_some_and(|character| character.is_ascii_whitespace()) {
            return false;
        }
        if is_shell_marker(character) {
            return true;
        }
    }
    false
}

fn has_windows_cmd_prompt(row: &[TerminalCell], host_end: usize) -> bool {
    let Some(path_start) = next_non_blank(row, host_end) else {
        return false;
    };
    if !ascii_cell(row, path_start).is_some_and(|character| character.is_ascii_alphabetic())
        || ascii_cell(row, path_start + 1) != Some(b':')
        || !matches!(ascii_cell(row, path_start + 2), Some(b'\\' | b'/'))
    {
        return false;
    }
    (path_start + 3..row.len())
        .take(512)
        .find(|index| ascii_cell(row, *index) == Some(b'>'))
        // CMD 会把用户输入紧接在 `>` 后渲染；提示符本身仍然有效，不能
        // 因为后面已经有命令文本就取消用户名/主机名高亮。
        .is_some()
}

fn identifier_start(row: &[TerminalCell], end: usize) -> usize {
    let mut start = end;
    while start > 0 && is_identifier(ascii_cell(row, start - 1)) {
        start -= 1;
    }
    start
}

fn identifier_end(row: &[TerminalCell], start: usize) -> usize {
    let mut end = start;
    while end < row.len() && is_identifier(ascii_cell(row, end)) {
        end += 1;
    }
    end
}

fn next_non_blank(row: &[TerminalCell], start: usize) -> Option<usize> {
    row.iter()
        .enumerate()
        .skip(start)
        .find(|(_, cell)| !cell.text.trim().is_empty())
        .map(|(index, _)| index)
}

fn is_identifier(character: Option<u8>) -> bool {
    character.is_some_and(|character| {
        character.is_ascii_alphanumeric() || matches!(character, b'_' | b'-' | b'.')
    })
}

fn is_shell_marker(character: Option<u8>) -> bool {
    matches!(character, Some(b'$' | b'#' | b'%' | b'>'))
}

fn color_range(colors: &mut [Option<RgbColor>], start: usize, end: usize, color: RgbColor) {
    for cell in colors.iter_mut().take(end).skip(start) {
        *cell = Some(color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::TerminalStyle;

    const USER: RgbColor = color(1);
    const HOST: RgbColor = color(2);
    const PUNCTUATION: RgbColor = color(3);

    #[test]
    fn colors_bracketed_and_plain_shell_prompts() {
        for text in [
            "[yuansuan@login10 /]$ pwd",
            "root@server:/srv# tail",
            r"administrator@CAE365BE C:\Users\Administrator>",
            r"administrator@CAE365BE C:\Users\Administrator> dir",
        ] {
            let rows = vec![row(text)];
            let mut colors = vec![vec![None; rows[0].len()]];
            color_shell_prompts(&rows, &mut colors, USER, HOST, PUNCTUATION);

            let at = text.find('@').expect("prompt should contain at sign");
            assert_eq!(colors[0][at - 1], Some(USER));
            assert_eq!(colors[0][at], Some(PUNCTUATION));
            assert_eq!(colors[0][at + 1], Some(HOST));
        }
    }

    #[test]
    fn does_not_color_email_or_output_without_prompt_marker() {
        for text in [
            "mail user@example.com",
            "user@example.com connected",
            "user@example.com price $5",
        ] {
            let rows = vec![row(text)];
            let mut colors = vec![vec![None; rows[0].len()]];
            color_shell_prompts(&rows, &mut colors, USER, HOST, PUNCTUATION);
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

    const fn color(red: u8) -> RgbColor {
        RgbColor {
            red,
            green: 0,
            blue: 0,
        }
    }
}
