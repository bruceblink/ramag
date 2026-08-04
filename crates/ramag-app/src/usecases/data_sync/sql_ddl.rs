//! SQL 同步的 DDL 重写。仅改写语法位置中的标识符，绝不对整段 SQL 做字符串替换。

use std::collections::HashMap;

use ramag_domain::entities::DriverKind;
use ramag_domain::error::{DomainError, Result};

pub(super) struct MysqlTableDdl {
    pub create_statement: String,
    pub foreign_key_statements: Vec<String>,
}

pub(super) fn qualified(driver: DriverKind, namespace: &str, object: &str) -> String {
    format!(
        "{}.{}",
        driver.quote_identifier(namespace),
        driver.quote_identifier(object)
    )
}

/// SHOW CREATE 的表头改为目标表，内联外键拆成最终阶段的 ALTER TABLE。
pub(super) fn rewrite_mysql_table_ddl(
    ddl: &str,
    source_namespace: &str,
    target_namespace: &str,
    source_table: &str,
    target_table: &str,
    mappings: &HashMap<String, String>,
) -> Result<MysqlTableDdl> {
    let open = find_unquoted(ddl, '(')
        .ok_or_else(|| DomainError::QueryFailed("MySQL CREATE TABLE 缺少列定义".into()))?;
    let close = matching_paren(ddl, open)
        .ok_or_else(|| DomainError::QueryFailed("MySQL CREATE TABLE 括号不完整".into()))?;
    let definitions = split_top_level(&ddl[open + 1..close], ',')?;
    let mut retained = Vec::with_capacity(definitions.len());
    let mut foreign_keys = Vec::new();
    let target = qualified(DriverKind::Mysql, target_namespace, target_table);
    for definition in definitions {
        let trimmed = definition.trim();
        if contains_keyword(trimmed, "FOREIGN KEY") {
            let mapped = rewrite_reference_target(
                trimmed,
                '`',
                source_namespace,
                target_namespace,
                mappings,
            )?;
            foreign_keys.push(format!("ALTER TABLE {target} ADD {mapped};"));
        } else if !trimmed.is_empty() {
            retained.push(trimmed.to_string());
        }
    }
    if retained.is_empty() {
        return Err(DomainError::QueryFailed(format!(
            "MySQL 表 {source_namespace}.{source_table} 没有可创建的列定义"
        )));
    }
    let suffix = ddl[close + 1..].trim().trim_end_matches(';');
    let mut create_statement = format!("CREATE TABLE {target} (\n  {}\n)", retained.join(",\n  "));
    if !suffix.is_empty() {
        create_statement.push(' ');
        create_statement.push_str(suffix);
    }
    create_statement.push(';');
    Ok(MysqlTableDdl {
        create_statement,
        foreign_key_statements: foreign_keys,
    })
}

/// 改写 PG catalog 生成语句中的 schema 限定标识符。
/// 双引号字符串以外的单引号、注释和 dollar-quoted body 原样保留。
pub(super) fn rewrite_postgres_statement(
    statement: &str,
    source_namespace: &str,
    target_namespace: &str,
    mappings: &HashMap<String, String>,
) -> Result<String> {
    let mut output = String::with_capacity(statement.len() + 32);
    let mut index = 0;
    let bytes = statement.as_bytes();
    while index < bytes.len() {
        if bytes[index] == b'\'' {
            let end = quoted_end(statement, index, b'\'', true)?;
            let following = statement[end..].trim_start();
            let prefix = output.trim_end().to_ascii_uppercase();
            if following.starts_with("::regclass")
                || prefix.ends_with("SETVAL(")
                || prefix.ends_with("NEXTVAL(")
            {
                let decoded = statement[index + 1..end - 1].replace("''", "'");
                let mapped = rewrite_qualified_literal(
                    &decoded,
                    source_namespace,
                    target_namespace,
                    mappings,
                );
                output.push('\'');
                output.push_str(&mapped.replace('\'', "''"));
                output.push('\'');
            } else {
                output.push_str(&statement[index..end]);
            }
            index = end;
            continue;
        }
        if bytes[index] == b'$'
            && let Some((tag, end)) = dollar_quote(statement, index)
        {
            output.push_str(&statement[index..end]);
            index = end;
            if tag.is_empty() {
                continue;
            }
            continue;
        }
        if bytes[index..].starts_with(b"--") {
            let end = statement[index..]
                .find('\n')
                .map_or(bytes.len(), |offset| index + offset);
            output.push_str(&statement[index..end]);
            index = end;
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            let relative = statement[index + 2..]
                .find("*/")
                .ok_or_else(|| DomainError::QueryFailed("PostgreSQL DDL 注释未闭合".into()))?;
            let end = index + 2 + relative + 2;
            output.push_str(&statement[index..end]);
            index = end;
            continue;
        }
        if bytes[index] == b'"' {
            let first_end = quoted_end(statement, index, b'"', true)?;
            let first = decode_quoted_identifier(&statement[index..first_end], '"');
            let mut cursor = first_end;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'.' {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if cursor < bytes.len()
                    && let Some((second, second_end)) = postgres_identifier(statement, cursor)?
                    && first == source_namespace
                {
                    output.push_str(&DriverKind::Postgres.quote_identifier(target_namespace));
                    output.push_str(&statement[first_end..cursor]);
                    let mapped = if is_relation_position(&statement[..index]) {
                        mappings
                            .get(&second)
                            .map_or(second.as_str(), String::as_str)
                    } else {
                        second.as_str()
                    };
                    output.push_str(&DriverKind::Postgres.quote_identifier(mapped));
                    index = second_end;
                    continue;
                }
            }
            output.push_str(&statement[index..first_end]);
            index = first_end;
            continue;
        }
        let ch = statement[index..]
            .chars()
            .next()
            .ok_or_else(|| DomainError::QueryFailed("PostgreSQL DDL UTF-8 边界异常".into()))?;
        if postgres_identifier_start(ch) {
            let first_end = postgres_unquoted_identifier_end(statement, index);
            let first = &statement[index..first_end];
            let mut cursor = first_end;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'.' {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if cursor < bytes.len()
                    && let Some((second, second_end)) = postgres_identifier(statement, cursor)?
                    && first == source_namespace
                {
                    output.push_str(&DriverKind::Postgres.quote_identifier(target_namespace));
                    output.push_str(&statement[first_end..cursor]);
                    let mapped = if is_relation_position(&statement[..index]) {
                        mappings
                            .get(&second)
                            .map_or(second.as_str(), String::as_str)
                    } else {
                        second.as_str()
                    };
                    output.push_str(&DriverKind::Postgres.quote_identifier(mapped));
                    index = second_end;
                    continue;
                }
            }
            output.push_str(first);
            index = first_end;
            continue;
        }
        output.push(ch);
        index += ch.len_utf8();
    }
    rewrite_reference_target(&output, '"', source_namespace, target_namespace, mappings)
}

fn postgres_identifier(statement: &str, start: usize) -> Result<Option<(String, usize)>> {
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

fn postgres_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

fn postgres_unquoted_identifier_end(statement: &str, start: usize) -> usize {
    statement[start..]
        .char_indices()
        .find_map(|(offset, ch)| {
            (!(ch == '_' || ch == '$' || ch.is_alphanumeric())).then_some(start + offset)
        })
        .unwrap_or(statement.len())
}

fn is_relation_position(prefix: &str) -> bool {
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

fn rewrite_qualified_literal(
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

fn parse_leading_quoted_identifier(text: &str) -> Option<(String, &str)> {
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

fn find_unquoted(text: &str, needle: char) -> Option<usize> {
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

fn matching_paren(text: &str, open: usize) -> Option<usize> {
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

fn split_top_level(text: &str, separator: char) -> Result<Vec<String>> {
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

fn contains_keyword(text: &str, keyword: &str) -> bool {
    text.to_ascii_uppercase().contains(keyword)
}

/// 只改写 REFERENCES 后的关系名；列名、约束名、字符串字面量均不触碰。
fn rewrite_reference_target(
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

fn find_keyword_outside_quotes(text: &str, keyword: &str) -> Option<usize> {
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

fn parse_identifier(text: &str, start: usize, quote: char) -> Result<Option<(String, usize)>> {
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

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || byte >= 0x80
}

fn quoted_end(text: &str, start: usize, quote: u8, doubled_escape: bool) -> Result<usize> {
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

fn decode_quoted_identifier(text: &str, quote: char) -> String {
    text[quote.len_utf8()..text.len() - quote.len_utf8()]
        .replace(&format!("{quote}{quote}"), &quote.to_string())
}

fn dollar_quote(text: &str, start: usize) -> Option<(String, usize)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_ddl_removes_fk_and_maps_only_reference() {
        let ddl = "CREATE TABLE `orders` (\n `id` bigint NOT NULL,\n `note` varchar(40) DEFAULT 'REFERENCES `orders`',\n PRIMARY KEY (`id`),\n CONSTRAINT `fk_self` FOREIGN KEY (`id`) REFERENCES `orders` (`id`) ON DELETE CASCADE\n) ENGINE=InnoDB";
        let mappings = HashMap::from([("orders".into(), "orders_copy".into())]);
        let mapped =
            rewrite_mysql_table_ddl(ddl, "old_db", "new_db", "orders", "orders_copy", &mappings)
                .unwrap();
        assert!(
            mapped
                .create_statement
                .starts_with("CREATE TABLE `new_db`.`orders_copy`")
        );
        assert!(mapped.create_statement.contains("'REFERENCES `orders`'"));
        assert!(!mapped.create_statement.contains("CONSTRAINT `fk_self`"));
        assert_eq!(
            mapped.foreign_key_statements,
            [
                "ALTER TABLE `new_db`.`orders_copy` ADD CONSTRAINT `fk_self` FOREIGN KEY (`id`) REFERENCES `new_db`.`orders_copy` (`id`) ON DELETE CASCADE;"
            ]
        );
    }

    #[test]
    fn postgres_rewrite_preserves_literals_and_same_named_column() {
        let mappings = HashMap::from([("orders".into(), "orders_copy".into())]);
        let sql = "CREATE TABLE \"old\".\"orders\" (\"old\" text DEFAULT '\"old\".\"orders\"');";
        let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
        assert_eq!(
            mapped,
            "CREATE TABLE \"new\".\"orders_copy\" (\"old\" text DEFAULT '\"old\".\"orders\"');"
        );
    }

    #[test]
    fn postgres_rewrite_maps_unqualified_fk_reference() {
        let mappings = HashMap::from([("parent".into(), "parent_copy".into())]);
        let sql = "ALTER TABLE \"old\".\"child\" ADD FOREIGN KEY (id) REFERENCES \"parent\"(id);";
        let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
        assert_eq!(
            mapped,
            "ALTER TABLE \"new\".\"child\" ADD FOREIGN KEY (id) REFERENCES \"new\".\"parent_copy\"(id);"
        );
    }

    #[test]
    fn postgres_rewrite_maps_unquoted_catalog_identifiers() {
        let mappings = HashMap::from([
            ("child".into(), "child_copy".into()),
            ("parent".into(), "parent_copy".into()),
        ]);
        let foreign_key = "ALTER TABLE old.child ADD CONSTRAINT fk FOREIGN KEY (parent_id) REFERENCES old.parent(id);";
        let mapped = rewrite_postgres_statement(foreign_key, "old", "new", &mappings).unwrap();
        assert!(mapped.starts_with("ALTER TABLE \"new\".\"child_copy\""));
        assert!(mapped.contains("REFERENCES \"new\".\"parent_copy\"(id)"));

        let index = rewrite_postgres_statement(
            "CREATE INDEX idx_child ON old.child USING btree (id);",
            "old",
            "new",
            &mappings,
        )
        .unwrap();
        assert!(index.contains(" ON \"new\".\"child_copy\""));

        let enum_type = rewrite_postgres_statement(
            "CREATE TYPE old.status AS ENUM ('active');",
            "old",
            "new",
            &mappings,
        )
        .unwrap();
        assert!(enum_type.starts_with("CREATE TYPE \"new\".\"status\""));
    }

    #[test]
    fn postgres_rewrite_maps_regclass_but_not_text_default() {
        let mappings = HashMap::from([("orders_id_seq".into(), "copy_id_seq".into())]);
        let sql = "CREATE TABLE \"old\".\"orders\" (id bigint DEFAULT nextval('\"old\".\"orders_id_seq\"'::regclass), note text DEFAULT 'old.orders_id_seq');";
        let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
        assert!(mapped.contains("nextval('\"new\".\"copy_id_seq\"'::regclass)"));
        assert!(mapped.contains("DEFAULT 'old.orders_id_seq'"));
    }

    #[test]
    fn postgres_type_name_equal_to_table_is_not_renamed() {
        let mappings = HashMap::from([("orders".into(), "orders_copy".into())]);
        let sql = "CREATE TABLE \"old\".\"orders\" (state \"old\".\"orders\");";
        let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
        assert_eq!(
            mapped,
            "CREATE TABLE \"new\".\"orders_copy\" (state \"new\".\"orders\");"
        );
    }

    #[test]
    fn malformed_ddl_is_rejected() {
        let error = rewrite_mysql_table_ddl(
            "CREATE TABLE x (`id` int",
            "a",
            "b",
            "x",
            "x",
            &HashMap::new(),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("括号"));
    }
}
