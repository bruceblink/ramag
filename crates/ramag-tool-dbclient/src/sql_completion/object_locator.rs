use ramag_domain::entities::DriverKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word { quoted: bool },
    Symbol(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SqlToken {
    start: usize,
    end: usize,
    kind: TokenKind,
}

/// Parses a selected SQL identifier such as `users` or `public.users`.
pub(crate) fn parse_table_reference(value: &str) -> Option<(Option<String>, String)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let tokens = tokenize(value, None);
    let mut index = 0;
    let mut parts = Vec::new();
    while let Some(token) = tokens.get(index) {
        let TokenKind::Word { .. } = token.kind else {
            return None;
        };
        parts.push(clean_identifier(value, *token)?);
        index += 1;
        if index == tokens.len() {
            break;
        }
        if !matches!(tokens[index].kind, TokenKind::Symbol(b'.')) {
            return None;
        }
        index += 1;
    }
    qualified_parts(parts)
}

/// Finds the table reference containing a UTF-8 byte cursor in SQL text.
pub(crate) fn table_reference_at_cursor(
    sql: &str,
    cursor: usize,
    driver: Option<DriverKind>,
) -> Option<(Option<String>, String)> {
    let tokens = tokenize(sql, driver);
    let cursor = cursor.min(sql.len());
    let mut index = 0;
    while index < tokens.len() {
        if !is_table_keyword(sql, tokens[index]) {
            index += 1;
            continue;
        }

        let first_index = skip_table_modifiers(sql, &tokens, index + 1);
        let Some(first) = tokens.get(first_index) else {
            index += 1;
            continue;
        };
        if !matches!(first.kind, TokenKind::Word { .. }) {
            index += 1;
            continue;
        }

        let mut end_index = first_index;
        while tokens
            .get(end_index + 1)
            .is_some_and(|token| matches!(token.kind, TokenKind::Symbol(b'.')))
            && tokens
                .get(end_index + 2)
                .is_some_and(|token| matches!(token.kind, TokenKind::Word { .. }))
        {
            end_index += 2;
        }

        let start = tokens[first_index].start;
        let end = tokens[end_index].end;
        if (start..=end).contains(&cursor)
            && let Some(reference) = parse_table_reference(&sql[start..end])
        {
            return Some(reference);
        }
        index = end_index.saturating_add(1);
    }
    None
}

fn qualified_parts(mut parts: Vec<(String, bool)>) -> Option<(Option<String>, String)> {
    if parts.is_empty() {
        return None;
    }
    let (table, quoted) = parts.pop()?;
    if !quoted && is_reserved_table_word(&table) {
        return None;
    }
    let schema = parts.pop().map(|(schema, _)| schema);
    Some((schema, table))
}

fn is_reserved_table_word(value: &str) -> bool {
    [
        "SELECT", "FROM", "JOIN", "INTO", "UPDATE", "TABLE", "VIEW", "USING",
    ]
    .iter()
    .any(|word| value.eq_ignore_ascii_case(word))
}

fn clean_identifier(value: &str, token: SqlToken) -> Option<(String, bool)> {
    let raw = &value[token.start..token.end];
    match token.kind {
        TokenKind::Word { quoted: false } => {
            is_unquoted_identifier(raw).then(|| (raw.to_string(), false))
        }
        TokenKind::Word { quoted: true } => unquote_identifier(raw).map(|value| (value, true)),
        TokenKind::Symbol(_) => None,
    }
}

fn is_unquoted_identifier(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(is_identifier_byte)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || byte >= 0x80
}

fn unquote_identifier(value: &str) -> Option<String> {
    let (open, close) = match (value.as_bytes().first()?, value.as_bytes().last()?) {
        (b'"', b'"') | (b'`', b'`') => (value.as_bytes()[0], value.as_bytes()[0]),
        (b'[', b']') => (b'[', b']'),
        _ => return None,
    };
    if value.len() < 2 {
        return None;
    }
    let body = &value[1..value.len() - 1];
    let escaped = match open {
        b'"' => body.replace("\"\"", "\""),
        b'`' => body.replace("``", "`"),
        b'[' if close == b']' => body.replace("]]", "]"),
        _ => body.to_string(),
    };
    (!escaped.is_empty()).then_some(escaped)
}

fn is_table_keyword(sql: &str, token: SqlToken) -> bool {
    let TokenKind::Word { quoted: false } = token.kind else {
        return false;
    };
    ["FROM", "JOIN", "INTO", "UPDATE", "TABLE", "VIEW", "USING"]
        .iter()
        .any(|keyword| sql[token.start..token.end].eq_ignore_ascii_case(keyword))
}

fn skip_table_modifiers(sql: &str, tokens: &[SqlToken], mut index: usize) -> usize {
    loop {
        if tokens
            .get(index)
            .is_some_and(|token| word_is(sql, *token, "ONLY") || word_is(sql, *token, "LATERAL"))
        {
            index += 1;
            continue;
        }
        if tokens
            .get(index)
            .is_some_and(|token| word_is(sql, *token, "IF"))
        {
            if tokens
                .get(index + 1)
                .is_some_and(|token| word_is(sql, *token, "EXISTS"))
            {
                index += 2;
                continue;
            }
            if tokens
                .get(index + 1)
                .is_some_and(|token| word_is(sql, *token, "NOT"))
                && tokens
                    .get(index + 2)
                    .is_some_and(|token| word_is(sql, *token, "EXISTS"))
            {
                index += 3;
                continue;
            }
        }
        return index;
    }
}

fn word_is(sql: &str, token: SqlToken, expected: &str) -> bool {
    matches!(token.kind, TokenKind::Word { quoted: false })
        && sql[token.start..token.end].eq_ignore_ascii_case(expected)
}

fn tokenize(sql: &str, driver: Option<DriverKind>) -> Vec<SqlToken> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index = skip_line_comment(bytes, index + 2);
            }
            b'#' if matches!(driver, Some(DriverKind::Mysql)) => {
                index = skip_line_comment(bytes, index + 1);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index);
            }
            b'\'' => {
                index = scan_quoted(bytes, index, b'\'');
            }
            b'"' | b'`' | b'[' => {
                let quote = bytes[index];
                let end = scan_quoted(bytes, index, quote);
                tokens.push(SqlToken {
                    start: index,
                    end,
                    kind: TokenKind::Word { quoted: true },
                });
                index = end;
            }
            b'$' if matches!(driver, Some(DriverKind::Postgres)) => {
                if let Some(end) = ramag_infra_sql_shared::sql::scan_dollar_quoted(bytes, index) {
                    index = end;
                } else {
                    let start = index;
                    index = scan_word(bytes, index);
                    tokens.push(SqlToken {
                        start,
                        end: index,
                        kind: TokenKind::Word { quoted: false },
                    });
                }
            }
            byte if is_identifier_byte(byte) => {
                let start = index;
                index = scan_word(bytes, index);
                tokens.push(SqlToken {
                    start,
                    end: index,
                    kind: TokenKind::Word { quoted: false },
                });
            }
            byte => {
                tokens.push(SqlToken {
                    start: index,
                    end: index + 1,
                    kind: TokenKind::Symbol(byte),
                });
                index += 1;
            }
        }
    }
    tokens
}

fn scan_word(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && is_identifier_byte(bytes[index]) {
        index += 1;
    }
    index
}

fn scan_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let close = if quote == b'[' { b']' } else { quote };
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == close {
            if bytes.get(index + 1) == Some(&close) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        if bytes[index] == b'\\' && quote != b'[' {
            index = (index + 2).min(bytes.len());
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn skip_line_comment(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 2;
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}
