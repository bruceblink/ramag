//! Redis CLI 命令行分词。

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// 将命令行切为 argv，支持引号和常用转义。
pub fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        // 裸段和引号段可拼接，如 foo"bar"。
        let mut current = String::new();
        loop {
            match chars.peek().copied() {
                None => break,
                Some(c) if c.is_whitespace() => break,
                Some('"') => {
                    chars.next();
                    parse_double_quoted(&mut chars, &mut current)?;
                }
                Some('\'') => {
                    chars.next();
                    parse_single_quoted(&mut chars, &mut current)?;
                }
                Some(c) => {
                    current.push(c);
                    chars.next();
                }
            }
        }
        args.push(current);
    }
    Ok(args)
}

fn parse_double_quoted(chars: &mut Chars, out: &mut String) -> Result<(), String> {
    loop {
        match chars.next() {
            None => return Err("双引号未闭合".into()),
            Some('"') => return Ok(()),
            Some('\\') => match chars.next() {
                None => return Err("双引号未闭合".into()),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('x') => {
                    let first = chars.next().ok_or("\\x 需两位十六进制")?;
                    let second = chars.next().ok_or("\\x 需两位十六进制")?;
                    let high =
                        hex_nibble(first).ok_or_else(|| "\\x 后须为两位十六进制".to_string())?;
                    let low =
                        hex_nibble(second).ok_or_else(|| "\\x 后须为两位十六进制".to_string())?;
                    let byte = (high << 4) | low;
                    if !byte.is_ascii() {
                        return Err(
                            "当前命令行仅支持 UTF-8 参数，\\xHH 不能表示 80-FF 原始字节".into()
                        );
                    }
                    out.push(byte as char);
                }
                Some(other) => out.push(other),
            },
            Some(c) => out.push(c),
        }
    }
}

fn hex_nibble(ch: char) -> Option<u8> {
    match ch {
        '0'..='9' => Some(ch as u8 - b'0'),
        'a'..='f' => Some(ch as u8 - b'a' + 10),
        'A'..='F' => Some(ch as u8 - b'A' + 10),
        _ => None,
    }
}

fn parse_single_quoted(chars: &mut Chars, out: &mut String) -> Result<(), String> {
    loop {
        match chars.next() {
            None => return Err("单引号未闭合".into()),
            Some('\'') => return Ok(()),
            Some('\\') if matches!(chars.peek(), Some('\'')) => {
                chars.next();
                out.push('\'');
            }
            Some(c) => out.push(c),
        }
    }
}
