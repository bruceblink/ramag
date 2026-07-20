use super::*;

#[test]
fn short_title_truncate() {
    assert_eq!(make_short_title("SELECT 1"), "SELECT 1");
    assert_eq!(
        make_short_title("SELECT * FROM very_long_table_name_here"),
        "SELECT * FROM very_long_tabl…"
    );
}

#[test]
fn short_title_skips_blank_lines() {
    let sql = "\n\n  -- comment\nSELECT 1";
    assert_eq!(make_short_title(sql), "-- comment");
}

#[test]
fn short_title_empty() {
    assert_eq!(make_short_title(""), "");
    assert_eq!(make_short_title("   "), "");
}

/// EXPLAIN 包装策略：模拟 actions.rs::handle_explain 的 SQL 处理
fn wrap_explain(sql: &str) -> String {
    let trimmed = sql.trim().trim_end_matches(';').trim().to_string();
    if trimmed.is_empty() {
        return String::new();
    }
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("EXPLAIN ") || upper == "EXPLAIN" {
        trimmed
    } else {
        format!("EXPLAIN {trimmed}")
    }
}

#[test]
fn parse_mysql_line() {
    assert_eq!(
        parse_mysql_error_line("You have an error in your SQL syntax... near 'foo' at line 3"),
        Some(3)
    );
    assert_eq!(parse_mysql_error_line("connection refused"), None);
    assert_eq!(parse_mysql_error_line("error at line 12"), Some(12));
}

#[test]
fn parse_postgres_line() {
    assert_eq!(
        parse_mysql_error_line("syntax error at end of input\nLINE 5: SELECT *"),
        Some(5)
    );
    assert_eq!(parse_mysql_error_line("LINE 1: SELECT * FORM t"), Some(1));
}

use ramag_domain::entities::DriverKind;

#[test]
fn extract_stmt_postgres_dollar_quoted_keeps_function_body_intact() {
    let sql =
        "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT 2";
    let pg = Some(DriverKind::Postgres);
    let stmt = extract_statement_at_cursor(sql, 45, pg);
    assert!(stmt.contains("CREATE FUNCTION"));
    assert!(stmt.contains("END"));
}

#[test]
fn extract_stmt_postgres_dollar_quoted_picks_next_statement() {
    let sql =
        "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT 2";
    let pg = Some(DriverKind::Postgres);
    let stmt = extract_statement_at_cursor(sql, sql.len() - 1, pg).trim();
    assert_eq!(stmt, "SELECT 2");
}

#[test]
fn extract_stmt_single() {
    assert_eq!(
        extract_statement_at_cursor("SELECT 1", 5, None).trim(),
        "SELECT 1"
    );
}

#[test]
fn extract_stmt_multi_picks_by_cursor() {
    let sql = "SELECT 1; SELECT 2; SELECT 3";
    assert_eq!(extract_statement_at_cursor(sql, 3, None).trim(), "SELECT 1");
    assert_eq!(
        extract_statement_at_cursor(sql, 12, None).trim(),
        "SELECT 2"
    );
    assert_eq!(
        extract_statement_at_cursor(sql, 25, None).trim(),
        "SELECT 3"
    );
}

#[test]
fn extract_stmt_ignores_semicolon_in_string() {
    let sql = "SELECT 'a;b'; SELECT 2";
    assert_eq!(
        extract_statement_at_cursor(sql, 5, None).trim(),
        "SELECT 'a;b'"
    );
    assert_eq!(
        extract_statement_at_cursor(sql, 18, None).trim(),
        "SELECT 2"
    );
}

#[test]
fn extract_stmt_ignores_semicolon_in_comment() {
    let sql = "SELECT 1 -- comment ;\n; SELECT 2";
    let first = extract_statement_at_cursor(sql, 5, None);
    assert!(first.contains("SELECT 1"));
    assert!(!first.contains("SELECT 2"));
    assert_eq!(
        extract_statement_at_cursor(sql, 26, None).trim(),
        "SELECT 2"
    );
}

#[test]
fn explain_wraps_plain_select() {
    assert_eq!(wrap_explain("SELECT 1"), "EXPLAIN SELECT 1");
    assert_eq!(
        wrap_explain("SELECT * FROM t WHERE id=1;"),
        "EXPLAIN SELECT * FROM t WHERE id=1"
    );
}

#[test]
fn explain_does_not_double_wrap() {
    assert_eq!(wrap_explain("EXPLAIN SELECT 1"), "EXPLAIN SELECT 1");
    assert_eq!(wrap_explain("explain  SELECT 1"), "explain  SELECT 1");
}

#[test]
fn explain_strips_trailing_semicolons() {
    assert_eq!(wrap_explain("SELECT 1;;;"), "EXPLAIN SELECT 1");
}

#[test]
fn dangerous_detects_delete_update_without_where() {
    let risks = detect_dangerous_statements("DELETE FROM t", DriverKind::Mysql);
    assert_eq!(risks.len(), 1);
    assert!(risks[0].contains("DELETE"));
    let risks = detect_dangerous_statements("update t set a=1", DriverKind::Mysql);
    assert_eq!(risks.len(), 1);
    assert!(risks[0].contains("UPDATE"));
}

#[test]
fn dangerous_allows_where_and_plain_statements() {
    assert!(detect_dangerous_statements("DELETE FROM t WHERE id=1", DriverKind::Mysql).is_empty());
    assert!(
        detect_dangerous_statements("UPDATE t SET a=1 WHERE id=1", DriverKind::Mysql).is_empty()
    );
    assert!(detect_dangerous_statements("SELECT * FROM t", DriverKind::Mysql).is_empty());
    assert!(detect_dangerous_statements("INSERT INTO t VALUES (1)", DriverKind::Mysql).is_empty());
}

#[test]
fn dangerous_detects_drop_truncate() {
    assert_eq!(
        detect_dangerous_statements("DROP TABLE t", DriverKind::Mysql).len(),
        1
    );
    assert_eq!(
        detect_dangerous_statements("TRUNCATE TABLE t", DriverKind::Postgres).len(),
        1
    );
}

#[test]
fn dangerous_skips_leading_comments_and_multi_statements() {
    // 注释开头不能骗过检测；多语句逐条检测
    let sql = "-- 清理\nDELETE FROM t;\nSELECT 1;\n/* x */ TRUNCATE t2";
    let risks = detect_dangerous_statements(sql, DriverKind::Mysql);
    assert_eq!(risks.len(), 2);
}

#[test]
fn dangerous_ignores_where_inside_dialect_comments() {
    assert_eq!(
        detect_dangerous_statements(
            "UPDATE t SET x = 1 /* outer /* inner */ WHERE id = 1 */",
            DriverKind::Postgres,
        )
        .len(),
        1
    );
    assert_eq!(
        detect_dangerous_statements("UPDATE t SET x = 1 # WHERE id = 1", DriverKind::Mysql,).len(),
        1
    );
    assert_eq!(
        detect_dangerous_statements(
            "/* outer /* inner */ still comment */ DELETE FROM t",
            DriverKind::Postgres,
        )
        .len(),
        1
    );
}

#[test]
fn dangerous_checks_mysql_executable_comments() {
    assert_eq!(
        detect_dangerous_statements("/*! UPDATE t SET x = 1 */", DriverKind::Mysql).len(),
        1
    );
    assert!(
        detect_dangerous_statements("/*! UPDATE t SET x = 1 WHERE id = 1 */", DriverKind::Mysql,)
            .is_empty()
    );
}

#[test]
fn dangerous_where_with_subquery_counts_as_top_level() {
    let risks =
        detect_dangerous_statements("DELETE FROM t WHERE id IN (SELECT 1)", DriverKind::Mysql);
    assert!(risks.is_empty());
}

#[test]
fn dangerous_cte_detects_unscoped_main_and_nested_writes() {
    assert_eq!(
        detect_dangerous_statements(
            "WITH ids AS (SELECT id FROM source) DELETE FROM target",
            DriverKind::Postgres,
        )
        .len(),
        1
    );
    assert_eq!(
        detect_dangerous_statements(
            "WITH removed AS (DELETE FROM target RETURNING id) SELECT * FROM removed",
            DriverKind::Postgres,
        )
        .len(),
        1
    );
}

#[test]
fn dangerous_cte_accepts_where_at_the_matching_depth() {
    assert!(
        detect_dangerous_statements(
            "WITH removed AS (DELETE FROM target WHERE id = 1 RETURNING id) SELECT * FROM removed",
            DriverKind::Postgres,
        )
        .is_empty()
    );
    assert!(
        detect_dangerous_statements(
            "WITH ids AS (SELECT id FROM source WHERE active) UPDATE target SET x = 1 WHERE id IN (SELECT id FROM ids)",
            DriverKind::Postgres,
        )
        .is_empty()
    );
}

#[test]
fn dangerous_cte_ignores_keywords_inside_dollar_quoted_text() {
    assert!(
        detect_dangerous_statements(
            "WITH x AS (SELECT $$ DELETE FROM t $$ AS text) SELECT * FROM x",
            DriverKind::Postgres,
        )
        .is_empty()
    );
}

#[test]
fn dangerous_does_not_count_where_inside_postgres_dollar_quoted_text() {
    let risks = detect_dangerous_statements(
        "UPDATE t SET note = $$ WHERE id = 1 $$",
        DriverKind::Postgres,
    );
    assert_eq!(risks.len(), 1);
}

#[test]
fn dangerous_recognizes_keyword_followed_by_comment() {
    assert_eq!(
        detect_dangerous_statements("DELETE/* reason */ FROM t", DriverKind::Mysql).len(),
        1
    );
    assert_eq!(
        detect_dangerous_statements(
            "WITH/* reason */ ids AS (SELECT id FROM source) UPDATE target SET x = 1",
            DriverKind::Postgres,
        )
        .len(),
        1
    );
}

#[test]
fn dangerous_postgres_explain_analyze_checks_executed_statement() {
    assert_eq!(
        detect_dangerous_statements(
            "EXPLAIN (ANALYZE, BUFFERS) DELETE FROM t",
            DriverKind::Postgres,
        )
        .len(),
        1
    );
    assert!(detect_dangerous_statements("EXPLAIN DELETE FROM t", DriverKind::Postgres).is_empty());
}

#[test]
fn sqlformat_works() {
    let opts = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(2),
        uppercase: Some(true),
        lines_between_queries: 1,
        ignore_case_convert: None,
    };
    let formatted = sqlformat::format(
        "select id,name from users where id=1 order by name",
        &sqlformat::QueryParams::None,
        &opts,
    );
    assert!(formatted.contains("SELECT"));
    assert!(formatted.contains("FROM"));
    assert!(formatted.contains("WHERE"));
    assert!(formatted.lines().count() >= 3);
}
