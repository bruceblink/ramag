//! 方言中性的 SQL 文本工具。

use ramag_domain::error::{DomainError, Result};

/// 单次最多执行 5,000 条业务语句，并为 MySQL 外键前缀预留 1 条。
pub const MAX_SQL_STATEMENTS: usize = ramag_domain::entities::TRANSFER_BATCH_ITEMS + 1;

/// 多语句切分选项。
#[derive(Debug, Clone, Copy)]
pub struct SplitOptions {
    /// 识别 PG dollar-quoted：`$$..$$` / `$tag$..$tag$`
    pub dollar_quoted: bool,
}

impl SplitOptions {
    pub fn mysql() -> Self {
        Self {
            dollar_quoted: false,
        }
    }

    pub fn postgres() -> Self {
        Self {
            dollar_quoted: true,
        }
    }
}

#[cfg(test)]
fn split_statements(sql: &str, opts: SplitOptions) -> Vec<String> {
    split_statements_with_limit(sql, opts, usize::MAX).unwrap_or_default()
}

/// 有界切分；检测到第 `max_statements + 1` 条时在复制该语句前返回错误。
pub fn split_statements_bounded(
    sql: &str,
    opts: SplitOptions,
    max_statements: usize,
) -> Result<Vec<String>> {
    split_statements_with_limit(sql, opts, max_statements).map_err(|()| {
        DomainError::InvalidConfig(format!(
            "SQL 语句数量超过 {max_statements} 条安全上限，请拆分脚本后执行"
        ))
    })
}

fn split_statements_with_limit(
    sql: &str,
    opts: SplitOptions,
    max_statements: usize,
) -> std::result::Result<Vec<String>, ()> {
    let bytes = sql.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

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
            b'$' if opts.dollar_quoted => {
                if let Some(end) = scan_dollar_quoted(bytes, i) {
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
                i = (i + 2).min(bytes.len());
            }
            b';' => {
                let segment = sql[start..i].trim();
                if !segment.is_empty() {
                    if out.len() >= max_statements {
                        return Err(());
                    }
                    out.push(segment.to_string());
                }
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let tail = sql[start..].trim();
    if !tail.is_empty() {
        if out.len() >= max_statements {
            return Err(());
        }
        out.push(tail.to_string());
    }
    Ok(out)
}

/// 返回 dollar-quoted 闭合标签后的字节位置；不支持嵌套。
pub fn scan_dollar_quoted(bytes: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(bytes[start], b'$');
    let mut p = start + 1;
    while p < bytes.len() && (bytes[p].is_ascii_alphanumeric() || bytes[p] == b'_') {
        p += 1;
    }
    if p >= bytes.len() || bytes[p] != b'$' {
        return None;
    }
    let tag_end = p;
    let body_start = tag_end + 1;
    let tag = &bytes[start..=tag_end];

    let mut q = body_start;
    while q + tag.len() <= bytes.len() {
        if &bytes[q..q + tag.len()] == tag {
            return Some(q + tag.len());
        }
        q += 1;
    }
    None
}

/// 按首关键字粗判 SQL 是否返回结果集。
pub fn is_query_returning_rows(sql: &str) -> bool {
    let code = sql_code_for_write_check(sql);
    let Some(keyword) = first_keyword(&code) else {
        return false;
    };
    matches!(
        keyword.as_str(),
        "SELECT" | "SHOW" | "DESC" | "DESCRIBE" | "EXPLAIN" | "WITH" | "VALUES" | "TABLE"
    ) || (matches!(keyword.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE")
        && contains_word(&code.to_ascii_uppercase(), "RETURNING"))
}

/// 返回首关键字（大写），跳过前导空白和注释。
pub fn first_keyword(stmt: &str) -> Option<String> {
    let bytes = stmt.as_bytes();
    let mut i = 0usize;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        break;
    }
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    (i > start).then(|| stmt[start..i].to_ascii_uppercase())
}

/// 生产模式只读保护：无法确认只读时一律拦截。
/// 白名单避免可执行注释、匿名代码块及 `SELECT INTO/OUTFILE` 绕过。
pub fn is_write_statement(stmt: &str) -> bool {
    // 白名单不含会修改会话或事务状态的命令。
    const SAFE_LEADING: &[&str] = &[
        "SELECT", "SHOW", "DESC", "DESCRIBE", "EXPLAIN", "WITH", "VALUES", "TABLE",
    ];
    // 安全首词仍扫描写动词，覆盖 WITH、EXPLAIN 和 SELECT INTO 等嵌套写入。
    const WRITE_INNER: &[&str] = &[
        "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "CREATE", "DROP", "ALTER", "TRUNCATE",
        "GRANT", "REVOKE", "OUTFILE", "DUMPFILE",
    ];
    let code = sql_code_for_write_check(stmt);
    let upper = code.to_ascii_uppercase();
    let Some(kw) = first_keyword(&upper) else {
        // 可执行注释含写动词时仍须拦截。
        return WRITE_INNER.iter().any(|w| contains_word(&upper, w));
    };
    if !SAFE_LEADING.contains(&kw.as_str()) {
        // 非白名单语句一律视为写。
        return true;
    }
    // SHOW CREATE TABLE 的 CREATE 是结果类型，不是执行动作。
    let show_create = kw == "SHOW"
        && upper
            .trim_start()
            .get(kw.len()..)
            .and_then(first_keyword)
            .is_some_and(|second| second == "CREATE");
    // 安全首词的语句体含写动词时仍视为写。
    if WRITE_INNER
        .iter()
        .any(|word| !(show_create && *word == "CREATE") && contains_word(&upper, word))
    {
        return true;
    }
    // PostgreSQL 的 SELECT INTO 会建表；MySQL 的 SELECT INTO @var 会修改会话状态。
    contains_word(&upper, "INTO")
}

/// 屏蔽字符串、引用标识符与普通注释，只保留数据库会执行的 SQL 代码。
/// MySQL `/*! ... */` 是可执行注释，保留其正文参与写操作检测。
fn sql_code_for_write_check(stmt: &str) -> String {
    let bytes = stmt.as_bytes();
    let mut code = bytes.to_vec();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"' | b'`') => {
                code[i] = b' ';
                i += 1;
                while i < bytes.len() {
                    code[i] = b' ';
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        code[i + 1] = b' ';
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            code[i + 1] = b' ';
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'$' => {
                if let Some(end) = scan_dollar_quoted(bytes, i) {
                    code[i..end].fill(b' ');
                    i = end;
                } else {
                    i += 1;
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    code[i] = b' ';
                    i += 1;
                }
            }
            b'/' if i + 2 < bytes.len() && bytes[i + 1] == b'*' && bytes[i + 2] == b'!' => {
                // 保留可执行注释正文参与检测。
                code[i..i + 3].fill(b' ');
                i += 3;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let start = i;
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                code[start..i].fill(b' ');
            }
            _ => i += 1,
        }
    }

    String::from_utf8_lossy(&code).into_owned()
}

/// 仅对未带 LIMIT 的 SELECT/WITH 注入 ` LIMIT n`；其他语句返回 None
pub fn inject_limit_if_needed(stmt: &str, limit: Option<u32>) -> Option<String> {
    let n = limit?;
    if n == 0 {
        return None;
    }
    let code = sql_code_for_write_check(stmt);
    if !first_keyword(&code).is_some_and(|keyword| matches!(keyword.as_str(), "SELECT" | "WITH")) {
        return None;
    }

    let mut tail_end = stmt.len();
    let bytes = stmt.as_bytes();
    while tail_end > 0 {
        let b = bytes[tail_end - 1];
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b';' {
            tail_end -= 1;
        } else {
            break;
        }
    }
    if tail_end == 0 {
        return None;
    }

    let core = &stmt[..tail_end];
    let masked = sql_code_for_write_check(core);
    let masked = masked.trim_end();
    let scan_start = masked.len().saturating_sub(256);
    let scan_str: String = masked
        .char_indices()
        .skip_while(|(i, _)| *i < scan_start)
        .map(|(_, c)| c)
        .collect();
    let upper = scan_str.to_ascii_uppercase();
    if contains_word(&upper, "LIMIT") {
        return None;
    }

    let trailing_semicolon = stmt[tail_end..].contains(';');
    let mut out = String::with_capacity(core.len() + 16);
    out.push_str(core);
    out.push_str(&format!(" LIMIT {n}"));
    if trailing_semicolon {
        out.push(';');
    }
    Some(out)
}

/// 是否含大小写不敏感的 `-- ramag:no-limit` 标记。
pub fn sql_has_no_limit_marker(sql: &str) -> bool {
    sql.lines().any(|line| {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            rest.trim_start()
                .to_ascii_lowercase()
                .starts_with("ramag:no-limit")
        } else {
            false
        }
    })
}

/// 全词匹配。
pub fn contains_word(haystack_upper: &str, word: &str) -> bool {
    let bytes = haystack_upper.as_bytes();
    let target = word.as_bytes();
    let mut i = 0;
    while i + target.len() <= bytes.len() {
        if &bytes[i..i + target.len()] == target {
            let before_ok = i == 0 || !is_word_byte(bytes[i - 1]);
            let after_ok =
                i + target.len() == bytes.len() || !is_word_byte(bytes[i + target.len()]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests;
