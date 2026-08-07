//! SQL 同步的 DDL 重写。仅改写语法位置中的标识符，绝不对整段 SQL 做字符串替换。
mod parsing;

use parsing::*;

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

#[cfg(test)]
mod tests;
