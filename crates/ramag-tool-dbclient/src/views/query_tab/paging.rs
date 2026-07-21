//! SQL 结果服务端分页：仅改写可安全识别的单条无界 SELECT/WITH。

use ramag_domain::entities::{DriverKind, MAX_SQL_QUERY_BYTES, QueryResult};
use ramag_infra_sql_shared::sql::{
    MAX_SQL_STATEMENTS, SplitOptions, first_keyword, is_write_statement, split_statements_bounded,
    sql_has_no_limit_marker,
};

use super::sql_utils::{has_top_level_keyword, strip_leading_comments};
use crate::views::result_panel::{MAX_ROWS_DISPLAY, TotalRows};

pub(super) const PAGE_SIZE: usize = MAX_ROWS_DISPLAY;

#[derive(Debug, Clone)]
pub(super) struct Pager {
    /// 去掉末尾分号但未注入分页语句的原始单条查询。
    pub(super) base_sql: String,
    /// 从 0 开始的当前页码。
    pub(super) page: usize,
    pub(super) has_more: bool,
    pub(super) page_size: usize,
    /// 全表精确总行数的异步计数状态：由首屏后台 COUNT 回填，翻页复用不重算。
    pub(super) total: TotalRows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageRequest {
    pub(super) page: usize,
    pub(super) page_size: usize,
}

/// 仅允许单条、只读且未显式分页的 SELECT/WITH 自动分页。
pub(super) fn paging_base_sql(sql: &str, driver: DriverKind) -> Option<String> {
    if !matches!(driver, DriverKind::Mysql | DriverKind::Postgres) || sql_has_no_limit_marker(sql) {
        return None;
    }
    let options = match driver {
        DriverKind::Postgres => SplitOptions::postgres(),
        _ => SplitOptions::mysql(),
    };
    let statements = split_statements_bounded(sql, options, MAX_SQL_STATEMENTS).ok()?;
    let [statement] = statements.as_slice() else {
        return None;
    };
    let trimmed = statement.trim();
    let body = strip_leading_comments(trimmed, driver);
    if !matches!(first_keyword(body).as_deref(), Some("SELECT" | "WITH"))
        || is_write_statement(body)
    {
        return None;
    }
    // 这些顶层子句已有分页语义，或要求处于语句末尾，不能再直接追加 LIMIT。
    if [
        "LIMIT",
        "OFFSET",
        "FETCH",
        "FOR",
        "LOCK",
        "PROCEDURE",
        "INTO",
    ]
    .iter()
    .any(|keyword| has_top_level_keyword(body, keyword, driver))
    {
        return None;
    }
    let base = trimmed.trim_end_matches(';').trim_end();
    (!base.is_empty()).then(|| base.to_string())
}

/// 多取一行作为“还有下一页”的哨兵；OFFSET 仍按可见页大小计算。
pub(super) fn page_sql(base_sql: &str, page_size: usize, page: usize) -> Result<String, String> {
    if page_size == 0 {
        return Err("分页大小必须大于 0".into());
    }
    let fetch_size = page_size
        .checked_add(1)
        .ok_or_else(|| "分页大小溢出".to_string())?;
    let offset = page
        .checked_mul(page_size)
        .ok_or_else(|| "分页偏移量溢出".to_string())?;
    // 换行可避免查询末尾的 -- 注释吞掉 LIMIT。
    let suffix = if offset == 0 {
        format!("\nLIMIT {fetch_size}")
    } else {
        format!("\nLIMIT {fetch_size} OFFSET {offset}")
    };
    let generated_len = base_sql
        .len()
        .checked_add(suffix.len())
        .ok_or_else(|| "分页 SQL 长度溢出".to_string())?;
    if generated_len > MAX_SQL_QUERY_BYTES {
        return Err(format!(
            "分页 SQL 超过 {} MiB 安全上限，请缩短原查询",
            MAX_SQL_QUERY_BYTES / 1024 / 1024
        ));
    }
    let mut generated = String::with_capacity(generated_len);
    generated.push_str(base_sql);
    generated.push_str(&suffix);
    Ok(generated)
}

/// 移除多取的哨兵行，并返回数据库是否还有下一页。
pub(super) fn trim_page_sentinel(result: &mut QueryResult, page_size: usize) -> bool {
    let has_more = result.rows.len() > page_size;
    if has_more {
        result.rows.truncate(page_size);
    }
    has_more
}

/// 把原始单条只读查询外包成子查询做精确计数。base_sql 已保证无 LIMIT/OFFSET/FOR
/// 等顶层子句（见 `paging_base_sql`），可安全外包 COUNT(*)。
pub(super) fn count_sql(base_sql: &str) -> Result<String, String> {
    // 前后换行：避免 base_sql 末尾行注释吞掉右括号与派生表别名。
    let prefix = "SELECT COUNT(*) FROM (\n";
    let suffix = "\n) AS ramag_total_count";
    let generated_len = prefix
        .len()
        .checked_add(base_sql.len())
        .and_then(|len| len.checked_add(suffix.len()))
        .ok_or_else(|| "计数 SQL 长度溢出".to_string())?;
    if generated_len > MAX_SQL_QUERY_BYTES {
        return Err(format!(
            "计数 SQL 超过 {} MiB 安全上限",
            MAX_SQL_QUERY_BYTES / 1024 / 1024
        ));
    }
    let mut generated = String::with_capacity(generated_len);
    generated.push_str(prefix);
    generated.push_str(base_sql);
    generated.push_str(suffix);
    Ok(generated)
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::{Row, Value};

    use super::*;

    fn query_result(row_count: usize) -> QueryResult {
        QueryResult {
            columns: vec!["id".into()],
            column_types: vec!["BIGINT".into()],
            rows: (0..row_count)
                .map(|index| Row {
                    values: vec![Value::Int(index as i64)],
                })
                .collect(),
            affected_rows: 0,
            elapsed_ms: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn plain_select_and_read_only_cte_are_eligible() {
        assert_eq!(
            paging_base_sql("SELECT * FROM t;", DriverKind::Mysql).as_deref(),
            Some("SELECT * FROM t")
        );
        assert!(
            paging_base_sql(
                "-- 列表\nWITH x AS (SELECT 1) SELECT * FROM x",
                DriverKind::Postgres,
            )
            .is_some()
        );
    }

    #[test]
    fn explicit_or_unsafe_paging_shapes_are_rejected() {
        for sql in [
            "SELECT * FROM t LIMIT 10",
            "SELECT * FROM t OFFSET 5",
            "SELECT * FROM t FETCH FIRST 10 ROWS ONLY",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t LOCK IN SHARE MODE",
            "SELECT id INTO copied_ids FROM t",
            "-- ramag:no-limit\nSELECT * FROM t",
            "SELECT 1; SELECT 2",
            "UPDATE t SET value = 1",
            "WITH changed AS (DELETE FROM t RETURNING id) SELECT * FROM changed",
        ] {
            assert!(
                paging_base_sql(sql, DriverKind::Postgres).is_none(),
                "{sql}"
            );
        }
    }

    #[test]
    fn subquery_limit_and_quoted_keywords_do_not_block_outer_paging() {
        assert!(
            paging_base_sql(
                "SELECT * FROM (SELECT * FROM t LIMIT 10) AS nested",
                DriverKind::Mysql,
            )
            .is_some()
        );
        assert!(
            paging_base_sql(
                "SELECT $$ LIMIT OFFSET FOR $$ AS text FROM t",
                DriverKind::Postgres,
            )
            .is_some()
        );
    }

    #[test]
    fn page_sql_fetches_one_sentinel_and_checks_arithmetic() {
        assert_eq!(
            page_sql("SELECT * FROM t", 10_000, 0).as_deref(),
            Ok("SELECT * FROM t\nLIMIT 10001")
        );
        assert_eq!(
            page_sql("SELECT * FROM t", 10_000, 2).as_deref(),
            Ok("SELECT * FROM t\nLIMIT 10001 OFFSET 20000")
        );
        assert!(page_sql("SELECT 1", usize::MAX, 0).is_err());
        assert!(page_sql("SELECT 1", 2, usize::MAX).is_err());
    }

    #[test]
    fn count_sql_wraps_base_query_as_derived_table() {
        assert_eq!(
            count_sql("SELECT * FROM t WHERE a > 1").as_deref(),
            Ok("SELECT COUNT(*) FROM (\nSELECT * FROM t WHERE a > 1\n) AS ramag_total_count")
        );
    }

    #[test]
    fn sentinel_is_removed_without_hiding_an_exact_page() {
        let mut exact = query_result(2);
        assert!(!trim_page_sentinel(&mut exact, 2));
        assert_eq!(exact.rows.len(), 2);

        let mut with_sentinel = query_result(3);
        assert!(trim_page_sentinel(&mut with_sentinel, 2));
        assert_eq!(with_sentinel.rows.len(), 2);
    }
}
