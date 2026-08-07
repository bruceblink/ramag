use super::*;

pub(super) fn postgres_identifier(
    statement: &str,
    start: usize,
) -> Result<Option<(String, usize)>> {
    let Some(ch) = statement[start..].chars().next() else {
        return Ok(None);
    };
    if ch == '"' {
        let end = quoted_end(statement, start, b'"', true)?;
        return Ok(Some((
            decode_quoted_identifier(&statement[start..end], '"'),
            end,
        )));
    }
    if !postgres_identifier_start(ch) {
        return Ok(None);
    }
    let end = postgres_unquoted_identifier_end(statement, start);
    Ok(Some((statement[start..end].to_string(), end)))
}

pub(super) fn postgres_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

pub(super) fn postgres_unquoted_identifier_end(statement: &str, start: usize) -> usize {
    statement[start..]
        .char_indices()
        .find_map(|(offset, ch)| {
            (!(ch == '_' || ch == '$' || ch.is_alphanumeric())).then_some(start + offset)
        })
        .unwrap_or(statement.len())
}

pub(super) fn is_relation_position(prefix: &str) -> bool {
    let upper = prefix.trim_end().to_ascii_uppercase();
    [
        "CREATE TABLE",
        "CREATE TABLE IF NOT EXISTS",
        "ALTER TABLE",
        "ALTER TABLE ONLY",
        "REFERENCES",
        " ON",
        " FROM",
        " JOIN",
        " UPDATE",
        " INTO",
        "COMMENT ON TABLE",
        "COMMENT ON COLUMN",
        "OWNED BY",
        "CREATE SEQUENCE",
        "CREATE SEQUENCE IF NOT EXISTS",
        "ALTER SEQUENCE",
    ]
    .iter()
    .any(|keyword| upper.ends_with(keyword))
}

pub(super) fn rewrite_qualified_literal(
    literal: &str,
    source_namespace: &str,
    target_namespace: &str,
    mappings: &HashMap<String, String>,
) -> String {
    let quoted_prefix = format!(
        "{}.",
        DriverKind::Postgres.quote_identifier(source_namespace)
    );
    if let Some(rest) = literal.strip_prefix(&quoted_prefix)
        && let Some((name, suffix)) = parse_leading_quoted_identifier(rest)
    {
        let mapped = mappings.get(&name).map_or(name.as_str(), String::as_str);
        return format!(
            "{}.{}{}",
            DriverKind::Postgres.quote_identifier(target_namespace),
            DriverKind::Postgres.quote_identifier(mapped),
            suffix
        );
    }
    let plain_prefix = format!("{source_namespace}.");
    if let Some(rest) = literal.strip_prefix(&plain_prefix) {
        let split = rest
            .find(|ch: char| !(ch == '_' || ch == '$' || ch.is_alphanumeric()))
            .unwrap_or(rest.len());
        let (name, suffix) = rest.split_at(split);
        if !name.is_empty() {
            let mapped = mappings.get(name).map_or(name, String::as_str);
            return format!("{target_namespace}.{mapped}{suffix}");
        }
    }
    literal.to_string()
}

pub(super) fn parse_leading_quoted_identifier(text: &str) -> Option<(String, &str)> {
    let body = text.strip_prefix('"')?;
    let mut name = String::new();
    let mut chars = body.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '"' {
            if chars.peek().is_some_and(|(_, next)| *next == '"') {
                chars.next();
                name.push('"');
            } else {
                return Some((name, &body[index + 1..]));
            }
        } else {
            name.push(ch);
        }
    }
    None
}

pub(super) fn find_unquoted(text: &str, needle: char) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0usize;
    let mut iter = text.char_indices().peekable();
    while let Some((index, ch)) = iter.next() {
        if let Some(active) = quote {
            if ch == active {
                if iter.peek().is_some_and(|(_, next)| *next == active) {
                    iter.next();
                } else {
                    quote = None;
                }
            } else if active == '\'' && ch == '\\' {
                iter.next();
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '(' => {
                if needle == '(' && depth == 0 {
                    return Some(index);
                }
                depth += 1;
            }
            ')' => depth = depth.saturating_sub(1),
            _ if ch == needle && depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

pub(super) fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut iter = text[open..].char_indices().peekable();
    while let Some((offset, ch)) = iter.next() {
        if let Some(active) = quote {
            if ch == active {
                if iter.peek().is_some_and(|(_, next)| *next == active) {
                    iter.next();
                } else {
                    quote = None;
                }
            } else if active == '\'' && ch == '\\' {
                iter.next();
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn split_top_level(text: &str, separator: char) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut quote = None;
    let mut iter = text.char_indices().peekable();
    while let Some((index, ch)) = iter.next() {
        if let Some(active) = quote {
            if ch == active {
                if iter.peek().is_some_and(|(_, next)| *next == active) {
                    iter.next();
                } else {
                    quote = None;
                }
            } else if active == '\'' && ch == '\\' {
                iter.next();
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| DomainError::QueryFailed("DDL 列定义括号层级异常".into()))?;
            }
            _ if ch == separator && depth == 0 => {
                result.push(text[start..index].to_string());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(DomainError::QueryFailed("DDL 列定义未闭合".into()));
    }
    result.push(text[start..].to_string());
    Ok(result)
}

pub(super) fn contains_keyword(text: &str, keyword: &str) -> bool {
    text.to_ascii_uppercase().contains(keyword)
}

/// 只改写 REFERENCES 后的关系名；列名、约束名、字符串字面量均不触碰。
pub(super) fn rewrite_reference_target(
    statement: &str,
    quote: char,
    source_namespace: &str,
    target_namespace: &str,
    mappings: &HashMap<String, String>,
) -> Result<String> {
    let Some(keyword) = find_keyword_outside_quotes(statement, "REFERENCES") else {
        return Ok(statement.to_string());
    };
    let mut start = keyword + "REFERENCES".len();
    while statement
        .as_bytes()
        .get(start)
        .is_some_and(u8::is_ascii_whitespace)
    {
        start += 1;
    }
    let Some((first, first_end)) = parse_identifier(statement, start, quote)? else {
        return Err(DomainError::QueryFailed(
            "外键 REFERENCES 后缺少合法表名".into(),
        ));
    };
    let mut cursor = first_end;
    while statement
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 1;
    }
    let (schema, table, end) = if statement.as_bytes().get(cursor) == Some(&b'.') {
        cursor += 1;
        while statement
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        let Some((second, second_end)) = parse_identifier(statement, cursor, quote)? else {
            return Err(DomainError::QueryFailed("外键限定表名不完整".into()));
        };
        (Some(first), second, second_end)
    } else {
        (None, first, first_end)
    };
    let should_map = schema
        .as_deref()
        .is_none_or(|value| value == source_namespace);
    if !should_map {
        return Ok(statement.to_string());
    }
    let mapped_table = mappings.get(&table).map_or(table.as_str(), String::as_str);
    let driver = if quote == '`' {
        DriverKind::Mysql
    } else {
        DriverKind::Postgres
    };
    let replacement = qualified(driver, target_namespace, mapped_table);
    Ok(format!(
        "{}{}{}",
        &statement[..start],
        replacement,
        &statement[end..]
    ))
}

pub(super) fn find_keyword_outside_quotes(text: &str, keyword: &str) -> Option<usize> {
    let upper = text.as_bytes();
    let keyword = keyword.as_bytes();
    let mut index = 0usize;
    let mut quote = None;
    while index + keyword.len() <= upper.len() {
        let byte = upper[index];
        if let Some(active) = quote {
            if byte == active {
                if upper.get(index + 1) == Some(&active) {
                    index += 2;
                    continue;
                }
                quote = None;
            } else if active == b'\'' && byte == b'\\' {
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if upper[index..index + keyword.len()].eq_ignore_ascii_case(keyword)
            && (index == 0 || !is_ident_byte(upper[index - 1]))
            && upper
                .get(index + keyword.len())
                .is_none_or(|next| !is_ident_byte(*next))
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

pub(super) fn parse_identifier(
    text: &str,
    start: usize,
    quote: char,
) -> Result<Option<(String, usize)>> {
    let Some(first) = text.as_bytes().get(start).copied() else {
        return Ok(None);
    };
    if first == quote as u8 {
        let end = quoted_end(text, start, quote as u8, true)?;
        return Ok(Some((
            decode_quoted_identifier(&text[start..end], quote),
            end,
        )));
    }
    if !is_ident_byte(first) {
        return Ok(None);
    }
    let mut end = start + 1;
    while text
        .as_bytes()
        .get(end)
        .is_some_and(|byte| is_ident_byte(*byte))
    {
        end += 1;
    }
    Ok(Some((text[start..end].to_string(), end)))
}

pub(super) fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || byte >= 0x80
}

pub(super) fn quoted_end(
    text: &str,
    start: usize,
    quote: u8,
    doubled_escape: bool,
) -> Result<usize> {
    let bytes = text.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            if doubled_escape && bytes.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return Ok(index + 1);
        }
        if quote == b'\'' && bytes[index] == b'\\' {
            index = index.saturating_add(2);
        } else {
            index += 1;
        }
    }
    Err(DomainError::QueryFailed("DDL 引号未闭合".into()))
}

pub(super) fn decode_quoted_identifier(text: &str, quote: char) -> String {
    text[quote.len_utf8()..text.len() - quote.len_utf8()]
        .replace(&format!("{quote}{quote}"), &quote.to_string())
}

pub(super) fn dollar_quote(text: &str, start: usize) -> Option<(String, usize)> {
    let rest = &text[start + 1..];
    let tag_end = rest.find('$')?;
    let tag = &rest[..tag_end];
    if !tag
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return None;
    }
    let delimiter = format!("${tag}$");
    let body_start = start + delimiter.len();
    let body_end = text[body_start..].find(&delimiter)? + body_start + delimiter.len();
    Some((tag.to_string(), body_end))
}
