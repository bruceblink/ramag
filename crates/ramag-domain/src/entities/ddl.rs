//! 生成查看表或视图 DDL 的查询。

use super::connection::DriverKind;

pub fn build_ddl_query(driver: DriverKind, schema: &str, table: &str, is_view: bool) -> String {
    let qschema = driver.quote_identifier(schema);
    let qtable = driver.quote_identifier(table);

    match driver {
        DriverKind::Mysql => {
            if is_view {
                format!("SHOW CREATE VIEW {qschema}.{qtable};")
            } else {
                format!("SHOW CREATE TABLE {qschema}.{qtable};")
            }
        }
        DriverKind::Postgres => {
            let s_lit = schema.replace('\'', "''");
            let t_lit = table.replace('\'', "''");
            if is_view {
                format!("SELECT pg_get_viewdef('\"{s_lit}\".\"{t_lit}\"'::regclass, true) AS ddl;")
            } else {
                postgres_table_ddl_sql(&s_lit, &t_lit)
            }
        }
        DriverKind::Sqlite => {
            let t_lit = table.replace('\'', "''");
            let object_type = if is_view { "view" } else { "table" };
            format!(
                "SELECT sql AS ddl FROM {qschema}.sqlite_schema WHERE type = '{object_type}' AND name = '{t_lit}';"
            )
        }
        DriverKind::Redis | DriverKind::Mongodb => String::new(),
    }
}

/// 参数必须已转义 SQL 字符串字面量中的单引号。
fn postgres_table_ddl_sql(s_lit: &str, t_lit: &str) -> String {
    format!(
        "WITH lines AS ( \
            SELECT a.attnum AS sort_key, \
                   '    \"' || a.attname || '\" ' || pg_catalog.format_type(a.atttypid, a.atttypmod) || \
                   CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END || \
                   CASE WHEN d.adbin IS NOT NULL THEN ' DEFAULT ' || pg_get_expr(d.adbin, d.adrelid) ELSE '' END AS line \
              FROM pg_attribute a \
              LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = '\"{s_lit}\".\"{t_lit}\"'::regclass \
               AND a.attnum > 0 AND NOT a.attisdropped \
            UNION ALL \
            SELECT 10000 + row_number() OVER (ORDER BY conname), \
                   '    CONSTRAINT \"' || conname || '\" ' || pg_get_constraintdef(oid) \
              FROM pg_constraint \
             WHERE conrelid = '\"{s_lit}\".\"{t_lit}\"'::regclass \
        ), \
        body AS ( \
            SELECT 'CREATE TABLE \"{s_lit}\".\"{t_lit}\" (' || E'\\n' || \
                   string_agg(line, E',\\n' ORDER BY sort_key) || \
                   E'\\n);' AS create_part \
              FROM lines \
        ), \
        idx AS ( \
            SELECT string_agg(pg_get_indexdef(i.indexrelid) || ';', E'\\n') AS idx_part \
              FROM pg_index i \
             WHERE i.indrelid = '\"{s_lit}\".\"{t_lit}\"'::regclass \
               AND NOT i.indisprimary \
               AND NOT EXISTS ( \
                 SELECT 1 FROM pg_constraint con \
                  WHERE con.conindid = i.indexrelid AND con.contype IN ('u','p','x') \
               ) \
        ), \
        tab_cmt AS ( \
            SELECT 'COMMENT ON TABLE \"{s_lit}\".\"{t_lit}\" IS ' || quote_literal(description) || ';' AS tc \
              FROM pg_description \
             WHERE objoid = '\"{s_lit}\".\"{t_lit}\"'::regclass AND objsubid = 0 \
        ), \
        col_cmt AS ( \
            SELECT string_agg( \
                     'COMMENT ON COLUMN \"{s_lit}\".\"{t_lit}\".\"' || a.attname || '\" IS ' || quote_literal(d.description) || ';', \
                     E'\\n' \
                   ) AS cc \
              FROM pg_attribute a \
              JOIN pg_description d ON d.objoid = a.attrelid AND d.objsubid = a.attnum \
             WHERE a.attrelid = '\"{s_lit}\".\"{t_lit}\"'::regclass \
        ) \
        SELECT body.create_part || \
               COALESCE(E'\\n\\n' || NULLIF(idx.idx_part, ''), '') || \
               COALESCE(E'\\n\\n' || NULLIF(tab_cmt.tc, ''), '') || \
               COALESCE(E'\\n' || NULLIF(col_cmt.cc, ''), '') AS ddl \
          FROM body LEFT JOIN idx ON true \
                    LEFT JOIN tab_cmt ON true \
                    LEFT JOIN col_cmt ON true;"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_table_ddl_uses_show_create() {
        let sql = build_ddl_query(DriverKind::Mysql, "mydb", "users", false);
        assert!(sql.starts_with("SHOW CREATE TABLE"));
        assert!(sql.contains("`mydb`"));
        assert!(sql.contains("`users`"));
    }

    #[test]
    fn mysql_view_ddl_uses_show_create_view() {
        let sql = build_ddl_query(DriverKind::Mysql, "mydb", "v1", true);
        assert!(sql.starts_with("SHOW CREATE VIEW"));
    }

    #[test]
    fn postgres_view_ddl_uses_pg_get_viewdef() {
        let sql = build_ddl_query(DriverKind::Postgres, "public", "v1", true);
        assert!(sql.contains("pg_get_viewdef"));
        assert!(sql.contains("\"public\".\"v1\""));
    }

    #[test]
    fn postgres_table_ddl_includes_constraints_and_indexes() {
        let sql = build_ddl_query(DriverKind::Postgres, "public", "users", false);
        assert!(sql.contains("pg_attribute"));
        assert!(sql.contains("pg_constraint"));
        assert!(sql.contains("pg_index"));
        assert!(sql.contains("pg_description"));
        assert!(sql.contains("pg_get_constraintdef"));
        assert!(sql.contains("pg_get_indexdef"));
        assert!(sql.contains("\"public\".\"users\""));
    }

    #[test]
    fn postgres_table_ddl_escapes_single_quote() {
        let sql = build_ddl_query(DriverKind::Postgres, "my'schema", "t", false);
        assert!(sql.contains("\"my''schema\".\"t\""));
        assert!(!sql.contains("\"my'schema\""));
    }
}
