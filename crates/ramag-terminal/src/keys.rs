//! 平台无关的常用终端按键编码。

use alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalKey {
    Character(String),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Function(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalModifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
}

pub fn encode_key(
    key: &TerminalKey,
    modifiers: TerminalModifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let mut encoded = match key {
        TerminalKey::Character(character) => encode_character(character, modifiers.control)?,
        TerminalKey::Enter => b"\r".to_vec(),
        TerminalKey::Backspace => vec![0x7f],
        TerminalKey::Tab if modifiers.shift => b"\x1b[Z".to_vec(),
        TerminalKey::Tab => b"\t".to_vec(),
        TerminalKey::Escape => b"\x1b".to_vec(),
        TerminalKey::Up => cursor_sequence('A', modifiers, mode),
        TerminalKey::Down => cursor_sequence('B', modifiers, mode),
        TerminalKey::Right => cursor_sequence('C', modifiers, mode),
        TerminalKey::Left => cursor_sequence('D', modifiers, mode),
        TerminalKey::Home => navigation_sequence('H', modifiers, mode),
        TerminalKey::End => navigation_sequence('F', modifiers, mode),
        TerminalKey::Insert => tilde_sequence(2, modifiers),
        TerminalKey::Delete => tilde_sequence(3, modifiers),
        TerminalKey::PageUp => tilde_sequence(5, modifiers),
        TerminalKey::PageDown => tilde_sequence(6, modifiers),
        TerminalKey::Function(number) => function_sequence(*number, modifiers)?,
    };
    // 特殊键已通过 xterm 修饰参数编码 Alt，只有字符键需要额外 ESC 前缀。
    if modifiers.alt && matches!(key, TerminalKey::Character(_)) {
        encoded.insert(0, 0x1b);
    }
    Some(encoded)
}

fn encode_character(character: &str, control: bool) -> Option<Vec<u8>> {
    if !control {
        return (!character.is_empty()).then(|| character.as_bytes().to_vec());
    }
    if character.chars().count() != 1 {
        return None;
    }
    let character = character.chars().next()?.to_ascii_lowercase();
    let control = match character {
        'a'..='z' => character as u8 - b'a' + 1,
        ' ' | '@' | '`' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    };
    Some(vec![control])
}

fn cursor_sequence(final_byte: char, modifiers: TerminalModifiers, mode: TermMode) -> Vec<u8> {
    if modifier_parameter(modifiers) == 1 && mode.contains(TermMode::APP_CURSOR) {
        format!("\x1bO{final_byte}").into_bytes()
    } else {
        navigation_sequence(final_byte, modifiers, mode)
    }
}

fn navigation_sequence(final_byte: char, modifiers: TerminalModifiers, _mode: TermMode) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter == 1 {
        format!("\x1b[{final_byte}").into_bytes()
    } else {
        format!("\x1b[1;{parameter}{final_byte}").into_bytes()
    }
}

fn tilde_sequence(number: u8, modifiers: TerminalModifiers) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{parameter}~").into_bytes()
    }
}

fn function_sequence(number: u8, modifiers: TerminalModifiers) -> Option<Vec<u8>> {
    let parameter = modifier_parameter(modifiers);
    if (1..=4).contains(&number) {
        let final_byte = char::from(b'P' + number - 1);
        return Some(if parameter == 1 {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[1;{parameter}{final_byte}").into_bytes()
        });
    }
    let code = match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(if parameter == 1 {
        format!("\x1b[{code}~").into_bytes()
    } else {
        format!("\x1b[{code};{parameter}~").into_bytes()
    })
}

fn modifier_parameter(modifiers: TerminalModifiers) -> u8 {
    1 + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt)
        + 4 * u8::from(modifiers.control)
        + 8 * u8::from(modifiers.platform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_and_cursor_keys_follow_terminal_conventions() {
        assert_eq!(
            encode_key(
                &TerminalKey::Character("c".into()),
                TerminalModifiers {
                    control: true,
                    ..Default::default()
                },
                TermMode::empty(),
            ),
            Some(vec![3])
        );
        assert_eq!(
            encode_key(
                &TerminalKey::Up,
                TerminalModifiers::default(),
                TermMode::APP_CURSOR,
            ),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(
                &TerminalKey::Right,
                TerminalModifiers {
                    control: true,
                    ..Default::default()
                },
                TermMode::empty(),
            ),
            Some(b"\x1b[1;5C".to_vec())
        );
    }

    #[test]
    fn tab_keys_are_forwarded_to_the_shell() {
        assert_eq!(
            encode_key(
                &TerminalKey::Tab,
                TerminalModifiers::default(),
                TermMode::empty(),
            ),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode_key(
                &TerminalKey::Tab,
                TerminalModifiers {
                    shift: true,
                    ..Default::default()
                },
                TermMode::empty(),
            ),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn function_key_bounds_are_explicit() {
        assert_eq!(
            encode_key(
                &TerminalKey::Function(12),
                TerminalModifiers::default(),
                TermMode::empty(),
            ),
            Some(b"\x1b[24~".to_vec())
        );
        assert!(
            encode_key(
                &TerminalKey::Function(13),
                TerminalModifiers::default(),
                TermMode::empty(),
            )
            .is_none()
        );
    }

    #[test]
    fn alt_is_not_encoded_twice_for_special_keys() {
        assert_eq!(
            encode_key(
                &TerminalKey::Left,
                TerminalModifiers {
                    alt: true,
                    ..Default::default()
                },
                TermMode::empty(),
            ),
            Some(b"\x1b[1;3D".to_vec())
        );
        assert_eq!(
            encode_key(
                &TerminalKey::Character("x".into()),
                TerminalModifiers {
                    alt: true,
                    ..Default::default()
                },
                TermMode::empty(),
            ),
            Some(b"\x1bx".to_vec())
        );
    }
}
