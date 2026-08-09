use super::*;

#[test]
fn split_single_no_semicolon() {
    assert_eq!(
        split_statements("SELECT 1", SplitOptions::mysql()),
        vec!["SELECT 1"]
    );
}

#[test]
fn split_skips_semicolon_in_string() {
    let s = split_statements("SELECT 'a;b'; SELECT 2", SplitOptions::mysql());
    assert_eq!(s, vec!["SELECT 'a;b'", "SELECT 2"]);
}

#[test]
fn split_skips_semicolon_in_line_comment() {
    let s = split_statements("SELECT 1 -- a;b\n; SELECT 2", SplitOptions::mysql());
    assert_eq!(s.len(), 2);
    assert_eq!(s[1], "SELECT 2");
}

#[test]
fn bounded_split_rejects_before_copying_an_extra_statement() {
    assert_eq!(
        split_statements_bounded("SELECT 1; SELECT 2", SplitOptions::mysql(), 2)
            .ok()
            .map(|statements| statements.len()),
        Some(2)
    );
    assert!(
        split_statements_bounded("SELECT 1; SELECT 2; SELECT 3", SplitOptions::mysql(), 2,)
            .is_err()
    );
}

#[test]
fn transfer_batch_and_mysql_internal_prefix_fit_statement_limit() {
    let mut sql = String::from("SET FOREIGN_KEY_CHECKS=0;");
    sql.push_str(&"INSERT INTO t VALUES (1);".repeat(ramag_domain::entities::TRANSFER_BATCH_ITEMS));

    assert_eq!(
        split_statements_bounded(&sql, SplitOptions::mysql(), MAX_SQL_STATEMENTS)
            .ok()
            .map(|statements| statements.len()),
        Some(MAX_SQL_STATEMENTS)
    );
    sql.push_str("SELECT 1;");
    assert!(split_statements_bounded(&sql, SplitOptions::mysql(), MAX_SQL_STATEMENTS).is_err());
}

#[test]
fn split_postgres_dollar_quoted_basic() {
    let sql =
        "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT 2";
    let s = split_statements(sql, SplitOptions::postgres());
    assert_eq!(s.len(), 2);
    assert!(s[0].contains("RETURN 1;"));
    assert_eq!(s[1], "SELECT 2");
}

#[test]
fn split_postgres_tagged_dollar_quoted() {
    let sql = "DO $body$ BEGIN PERFORM 1; END; $body$; SELECT 3";
    let s = split_statements(sql, SplitOptions::postgres());
    assert_eq!(s.len(), 2);
    assert_eq!(s[1], "SELECT 3");
}

#[test]
fn mysql_does_not_treat_dollar_as_quote() {
    let sql = "SELECT '$$abc$$'; SELECT 2";
    let s = split_statements(sql, SplitOptions::mysql());
    assert_eq!(s.len(), 2);
}

#[test]
fn inject_basic() {
    assert_eq!(
        inject_limit_if_needed("SELECT * FROM t", Some(500)).as_deref(),
        Some("SELECT * FROM t LIMIT 500")
    );
}

#[test]
fn inject_skip_when_already_has_limit() {
    assert!(inject_limit_if_needed("SELECT * FROM t LIMIT 10", Some(500)).is_none());
}

#[test]
fn detect_returning_rows() {
    assert!(is_query_returning_rows("SELECT 1"));
    assert!(is_query_returning_rows("VALUES (1, 2)"));
    assert!(is_query_returning_rows("-- comment\nSELECT 1"));
    assert!(is_query_returning_rows(
        "INSERT INTO t VALUES (1) RETURNING id"
    ));
    assert!(is_query_returning_rows(
        "UPDATE t SET value = 1 RETURNING *"
    ));
    assert!(!is_query_returning_rows("INSERT INTO t VALUES (1)"));
}

#[test]
fn no_limit_marker() {
    assert!(sql_has_no_limit_marker("-- ramag:no-limit\nSELECT 1"));
    assert!(!sql_has_no_limit_marker("SELECT 'ramag:no-limit'"));
}

#[test]
fn write_statement_dml_vs_select() {
    assert!(is_write_statement("INSERT INTO t VALUES (1)"));
    assert!(is_write_statement("  update t set x=1"));
    assert!(is_write_statement("DELETE FROM t"));
    assert!(!is_write_statement("SELECT 1"));
    assert!(!is_write_statement("select * from t"));
    assert!(!is_write_statement("SHOW TABLES"));
    assert!(!is_write_statement("SHOW CREATE TABLE `app`.`users`"));
    assert!(is_write_statement(
        "SHOW CREATE TABLE users /*! DELETE FROM audit_log */"
    ));
}

#[test]
fn write_statement_ignores_keywords_in_literals_and_comments() {
    assert!(!is_write_statement(
        "SELECT 'CREATE TABLE t(id int)' AS ddl"
    ));
    assert!(!is_write_statement("SELECT \"DELETE\" FROM t"));
    assert!(!is_write_statement("SELECT $$DROP TABLE t$$ AS ddl"));
    assert!(!is_write_statement(
        "WITH x AS (SELECT 'UPDATE t') SELECT * FROM x"
    ));
    assert!(!is_write_statement("SELECT 1 /* DROP TABLE t */"));
    assert!(!is_write_statement("/*! SELECT 'DROP TABLE t' */"));
}

#[test]
fn write_statement_ddl() {
    assert!(is_write_statement("DROP TABLE t"));
    assert!(is_write_statement("TRUNCATE TABLE t"));
    assert!(is_write_statement("create table t(id int)"));
    assert!(is_write_statement("ALTER TABLE t ADD c int"));
    assert!(is_write_statement("CALL proc()"));
}

#[test]
fn write_statement_skips_leading_comment() {
    assert!(is_write_statement("-- danger\nDELETE FROM t"));
    assert!(is_write_statement("/* x */ DROP TABLE t"));
    assert!(!is_write_statement("-- just a select\nSELECT 1"));
}

#[test]
fn write_statement_returning_is_write() {
    // PG：INSERT/UPDATE/DELETE ... RETURNING 会返回行但仍是写
    assert!(is_write_statement("INSERT INTO t VALUES (1) RETURNING id"));
    assert!(is_write_statement("UPDATE t SET x=1 RETURNING *"));
}

#[test]
fn write_statement_cte_and_explain() {
    // 纯读 CTE / EXPLAIN 放行；CTE 内含写、EXPLAIN ANALYZE 真执行写则拦
    assert!(!is_write_statement("WITH x AS (SELECT 1) SELECT * FROM x"));
    assert!(is_write_statement(
        "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x"
    ));
    assert!(!is_write_statement("EXPLAIN SELECT 1"));
    assert!(!is_write_statement("EXPLAIN ANALYZE SELECT 1"));
    assert!(is_write_statement(
        "EXPLAIN ANALYZE INSERT INTO t VALUES (1)"
    ));
}

#[test]
fn write_statement_blocks_session_state_and_allows_empty() {
    assert!(is_write_statement("SET names utf8"));
    assert!(is_write_statement("USE mydb"));
    assert!(is_write_statement("BEGIN"));
    assert!(is_write_statement("ANALYZE users"));
    assert!(!is_write_statement(""));
    assert!(!is_write_statement("   "));
}

#[test]
fn write_statement_blocks_known_bypasses() {
    // MySQL 可执行注释：/*! ... */ 内的 DELETE 会被 MySQL 执行，必须拦
    assert!(is_write_statement("/*! DELETE FROM t */"));
    assert!(is_write_statement("/*!40000 UPDATE t SET x=1 */"));
    // PG 匿名代码块：DO 首词不在白名单，当写
    assert!(is_write_statement("DO $$ BEGIN DELETE FROM t; END $$"));
    // SELECT ... INTO 建表 / OUTFILE 落盘：首词 SELECT 但含 INTO/OUTFILE
    assert!(is_write_statement("SELECT * INTO newtable FROM t"));
    assert!(is_write_statement(
        "SELECT * FROM t INTO OUTFILE '/tmp/x.csv'"
    ));
    // 未知首词一律当写（COPY/LOAD/纯注释含写动词）
    assert!(is_write_statement("COPY t FROM '/tmp/x.csv'"));
    assert!(is_write_statement("LOAD DATA INFILE '/x' INTO TABLE t"));
    // 正常只读不误伤
    assert!(!is_write_statement(
        "SELECT * FROM t WHERE name = 'no limit'"
    ));
    assert!(is_write_statement("SELECT id INTO @v FROM t"));
}

#[test]
fn limit_detection_ignores_literals_and_long_trailing_comments() {
    assert_eq!(
        inject_limit_if_needed("-- heading\nSELECT * FROM t", Some(5)).as_deref(),
        Some("-- heading\nSELECT * FROM t LIMIT 5")
    );
    assert_eq!(
        inject_limit_if_needed("SELECT 'LIMIT' AS note FROM t", Some(5)).as_deref(),
        Some("SELECT 'LIMIT' AS note FROM t LIMIT 5")
    );
    let sql = format!("SELECT * FROM t LIMIT 7 /* {} */", "x".repeat(300));
    assert!(inject_limit_if_needed(&sql, Some(5)).is_none());
}
