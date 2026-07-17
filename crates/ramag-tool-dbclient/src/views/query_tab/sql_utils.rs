//! query_tab 的 SQL 纯函数：LIMIT 注入 / 高危语句检测 / 多语句切分 / 光标处取语句 / 错误行号 / 短标题 / 耗时格式

use std::time::Duration;

/// 格式化运行中耗时：< 60s 显示 "X.Xs"，>= 60s 显示 "Mm Ss"
pub(super) fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let m = (secs / 60.0) as u64;
        let s = secs as u64 % 60;
        format!("{m}m {s}s")
    }
}

/// 给"裸 SELECT / SHOW / DESC"自动注入 LIMIT，避免误把全表拉回来。
/// 多语句时按 `;` 切分逐条处理；PG 切分时识别 dollar-quoted 函数体内的 `;`。
/// 已经有顶层 `LIMIT` / 非查询语句保持原样。
pub(crate) fn inject_limits(
    sql: &str,
    max_rows: usize,
    driver: ramag_domain::entities::DriverKind,
) -> String {
    let stmts = split_sql_statements(sql, driver);
    if stmts.is_empty() {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len() + 16 * stmts.len());
    for (i, stmt) in stmts.iter().enumerate() {
        let s = inject_limit_one(stmt, max_rows, driver);
        if i > 0 {
            out.push_str(";\n");
        }
        out.push_str(&s);
    }
    if sql.trim_end().ends_with(';') {
        out.push(';');
    }
    out
}

/// 单条语句 LIMIT 注入：仅 SELECT/WITH 类，且不含 LIMIT 时
fn inject_limit_one(
    stmt: &str,
    max_rows: usize,
    driver: ramag_domain::entities::DriverKind,
) -> String {
    let trimmed = stmt.trim();
    if trimmed.is_empty() {
        return stmt.to_string();
    }
    let upper: String = trimmed
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    if !(upper.starts_with("SELECT") || upper.starts_with("WITH")) {
        return stmt.to_string();
    }
    if has_top_level_keyword(trimmed, "LIMIT", driver) {
        return stmt.to_string();
    }
    let body = trimmed.trim_end_matches(';').trim_end();
    format!("{body} LIMIT {max_rows}")
}

/// 检测 SQL 中是否有顶层（不在括号子查询里）的关键字。
/// 词法扫描会跳过引号、注释与 PostgreSQL dollar-quoted 内容。
pub(super) fn has_top_level_keyword(
    sql: &str,
    keyword: &str,
    driver: ramag_domain::entities::DriverKind,
) -> bool {
    keyword_depths(sql, driver)
        .iter()
        .any(|(token, depth)| *depth == 0 && token == keyword)
}

/// 跳过语句开头的普通注释与空白，返回真正的语句体。
/// MySQL 可执行注释不能被丢弃，否则会绕过高危语句检测。
fn strip_leading_comments(stmt: &str, driver: ramag_domain::entities::DriverKind) -> &str {
    let mut s = stmt.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
        } else if matches!(driver, ramag_domain::entities::DriverKind::Mysql) && s.starts_with('#')
        {
            s = s.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
        } else if s.starts_with("/*") && !is_mysql_executable_comment(s.as_bytes(), 0, driver) {
            let end = block_comment_end(
                s.as_bytes(),
                0,
                matches!(driver, ramag_domain::entities::DriverKind::Postgres),
            );
            s = &s[end..];
        } else {
            return s;
        }
        s = s.trim_start();
    }
}

/// 高危语句检测：DELETE / UPDATE 无顶层 WHERE、DROP、TRUNCATE。
/// 返回命中语句的风险描述（供执行前确认弹框展示）；普通写操作（带 WHERE 的
/// UPDATE/DELETE、INSERT、ALTER 等）不拦，避免每次写操作都要确认
pub(super) fn detect_dangerous_statements(
    sql: &str,
    driver: ramag_domain::entities::DriverKind,
) -> Vec<String> {
    let mut risks = Vec::new();
    for stmt in split_sql_statements(sql, driver) {
        let body = strip_leading_comments(&stmt, driver);
        if body.is_empty() {
            continue;
        }
        let tokens = keyword_depths(body, driver);
        let Some(first) = tokens.first().map(|(keyword, _)| keyword.as_str()) else {
            continue;
        };
        let risk = if first == "WITH"
            || (first == "EXPLAIN"
                && matches!(driver, ramag_domain::entities::DriverKind::Postgres)
                && postgres_explain_executes(&tokens))
        {
            detect_dangerous_tokens(&tokens)
        } else {
            match first {
                "DELETE" if !has_top_level_keyword(body, "WHERE", driver) => {
                    Some("DELETE 未带 WHERE（将删除整表数据）")
                }
                "UPDATE" if !has_top_level_keyword(body, "WHERE", driver) => {
                    Some("UPDATE 未带 WHERE（将改写整表数据）")
                }
                "DROP" => Some("DROP（将删除表 / 库等对象，不可恢复）"),
                "TRUNCATE" => Some("TRUNCATE（将清空整表数据，不可回滚）"),
                _ => None,
            }
        };
        if let Some(r) = risk {
            risks.push(format!("{r}：{}", make_short_title(body)));
        }
    }
    risks
}

/// 逐层检查写关键字。`WHERE` 必须与对应 DELETE/UPDATE 处于同一括号深度；
/// 子查询中的 WHERE 不算保护条件。用于 CTE 与会实际执行语句的 EXPLAIN ANALYZE。
fn detect_dangerous_tokens(tokens: &[(String, i32)]) -> Option<&'static str> {
    for (index, (keyword, depth)) in tokens.iter().enumerate() {
        let risk = match keyword.as_str() {
            "DELETE" if !has_following_keyword_at_depth(tokens, index, *depth, "WHERE") => {
                Some("DELETE 未带 WHERE（将删除整表数据）")
            }
            "UPDATE" if !has_following_keyword_at_depth(tokens, index, *depth, "WHERE") => {
                Some("UPDATE 未带 WHERE（将改写整表数据）")
            }
            "DROP" => Some("DROP（将删除表 / 库等对象，不可恢复）"),
            "TRUNCATE" => Some("TRUNCATE（将清空整表数据，不可回滚）"),
            _ => None,
        };
        if risk.is_some() {
            return risk;
        }
    }
    None
}

/// PostgreSQL 只有 EXPLAIN ANALYZE 会真正执行目标语句。
fn postgres_explain_executes(tokens: &[(String, i32)]) -> bool {
    let statement_index =
        tokens
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(index, (keyword, depth))| {
                (*depth == 0
                    && matches!(
                        keyword.as_str(),
                        "SELECT"
                            | "WITH"
                            | "INSERT"
                            | "UPDATE"
                            | "DELETE"
                            | "MERGE"
                            | "VALUES"
                            | "TABLE"
                    ))
                .then_some(index)
            });
    let end = statement_index.unwrap_or(tokens.len());
    tokens[1..end]
        .iter()
        .any(|(keyword, _)| keyword == "ANALYZE")
}

fn has_following_keyword_at_depth(
    tokens: &[(String, i32)],
    start: usize,
    depth: i32,
    expected: &str,
) -> bool {
    for (keyword, token_depth) in tokens.iter().skip(start + 1) {
        if *token_depth < depth {
            break;
        }
        if *token_depth == depth && keyword == expected {
            return true;
        }
    }
    false
}

/// 跳过字符串、标识符引号、注释与 PostgreSQL dollar-quoted 内容，返回关键字及括号深度。
fn keyword_depths(sql: &str, driver: ramag_domain::entities::DriverKind) -> Vec<(String, i32)> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut depth = 0i32;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            quote @ (b'\'' | b'"' | b'`') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == quote {
                        if index + 1 < bytes.len() && bytes[index + 1] == quote {
                            index += 2;
                            continue;
                        }
                        index += 1;
                        break;
                    }
                    if bytes[index] == b'\\' && index + 1 < bytes.len() {
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
            }
            b'-' if index + 1 < bytes.len() && bytes[index + 1] == b'-' => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'#' if matches!(driver, ramag_domain::entities::DriverKind::Mysql) => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'*' => {
                if is_mysql_executable_comment(bytes, index, driver) {
                    index += if bytes.get(index + 2) == Some(&b'!') {
                        3
                    } else {
                        4
                    };
                } else {
                    index = block_comment_end(
                        bytes,
                        index,
                        matches!(driver, ramag_domain::entities::DriverKind::Postgres),
                    );
                }
            }
            b'$' if matches!(driver, ramag_domain::entities::DriverKind::Postgres) => {
                if let Some(end) = ramag_infra_sql_shared::sql::scan_dollar_quoted(bytes, index) {
                    index = end;
                } else {
                    index += 1;
                }
            }
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth = (depth - 1).max(0);
                index += 1;
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                tokens.push((sql[start..index].to_ascii_uppercase(), depth));
            }
            _ => index += 1,
        }
    }
    tokens
}

fn is_mysql_executable_comment(
    bytes: &[u8],
    start: usize,
    driver: ramag_domain::entities::DriverKind,
) -> bool {
    matches!(driver, ramag_domain::entities::DriverKind::Mysql)
        && (bytes.get(start..start + 3) == Some(b"/*!".as_slice())
            || bytes.get(start..start + 4) == Some(b"/*M!".as_slice()))
}

fn block_comment_end(bytes: &[u8], start: usize, nested: bool) -> usize {
    let mut index = start + 2;
    let mut depth = 1usize;
    while index + 1 < bytes.len() {
        if nested && bytes[index] == b'/' && bytes[index + 1] == b'*' {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    bytes.len()
}

/// 多语句切分：复用 sql-shared 的实现，按 driver 选择是否识别 PG dollar-quoted
pub(super) fn split_sql_statements(
    sql: &str,
    driver: ramag_domain::entities::DriverKind,
) -> Vec<String> {
    let opts = match driver {
        ramag_domain::entities::DriverKind::Postgres => {
            ramag_infra_sql_shared::sql::SplitOptions::postgres()
        }
        _ => ramag_infra_sql_shared::sql::SplitOptions::mysql(),
    };
    ramag_infra_sql_shared::sql::split_statements_bounded(
        sql,
        opts,
        ramag_infra_sql_shared::sql::MAX_SQL_STATEMENTS,
    )
    .unwrap_or_else(|_| vec![sql.to_string()])
}

/// MySQL `at line N` / PG `LINE N:` 格式提取行号
pub(crate) fn parse_mysql_error_line(msg: &str) -> Option<usize> {
    if let Some(idx) = msg.find(" at line ") {
        let tail = &msg[idx + " at line ".len()..];
        let num: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<usize>() {
            return Some(n);
        }
    }
    if let Some(idx) = msg.find("LINE ") {
        let tail = &msg[idx + "LINE ".len()..];
        let num: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<usize>() {
            return Some(n);
        }
    }
    None
}

/// 按 `;` 切分提取光标处语句，跳过字符串 / 行 / 块注释 / PG dollar-quoted 内的 `;`。
/// `cursor` 是 UTF-8 byte offset；越界按最后一条
pub(super) fn extract_statement_at_cursor(
    sql: &str,
    cursor: usize,
    driver: Option<ramag_domain::entities::DriverKind>,
) -> &str {
    let pg = matches!(driver, Some(ramag_domain::entities::DriverKind::Postgres));
    let bytes = sql.as_bytes();
    let cursor = cursor.min(bytes.len());
    let mut splits: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' | b'"' | b'`' => {
                let quote = b;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'$' if pg => {
                if let Some(end) = ramag_infra_sql_shared::sql::scan_dollar_quoted(bytes, i) {
                    i = end;
                } else {
                    i += 1;
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b';' => {
                splits.push(i);
                i += 1;
            }
            _ => i += 1,
        }
    }

    let mut start = 0;
    for &sp in &splits {
        if sp >= cursor {
            return safe_str_slice(sql, start, sp);
        }
        start = sp + 1;
    }
    safe_str_slice(sql, start, bytes.len())
}

fn safe_str_slice(sql: &str, mut start: usize, mut end: usize) -> &str {
    let bytes = sql.as_bytes();
    while start < bytes.len() && !sql.is_char_boundary(start) {
        start += 1;
    }
    while end > 0 && !sql.is_char_boundary(end) {
        end -= 1;
    }
    if end < start {
        return "";
    }
    &sql[start..end]
}

/// 从 SQL 派生短标题：取首条非空行前 28 个字符（按字符计，不按字节）
pub(super) fn make_short_title(sql: &str) -> String {
    const MAX: usize = 28;
    let first_line = sql
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    first_line.char_indices().nth(MAX).map_or_else(
        || first_line.to_string(),
        |(end, _)| format!("{}…", &first_line[..end]),
    )
}

#[cfg(test)]
mod tests;
