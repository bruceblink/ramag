//! 连接表单默认值。

pub(super) const DEFAULT_HOST: &str = "127.0.0.1";

pub(super) fn host_placeholder(driver_id: &str) -> &'static str {
    if driver_id == "sqlite" {
        "相对或绝对路径，如 ./data/app.db"
    } else {
        DEFAULT_HOST
    }
}

pub(super) fn default_port(driver_id: &str) -> u16 {
    match driver_id {
        "postgres" => 5432,
        "sqlite" => 0,
        "redis" => 6379,
        "mongodb" => 27017,
        _ => 3306,
    }
}

pub(super) fn default_username(driver_id: &str) -> &'static str {
    match driver_id {
        "mysql" => "root",
        "postgres" => "postgres",
        "sqlite" => "",
        _ => "",
    }
}

pub(super) fn username_placeholder(driver_id: &str) -> &'static str {
    match default_username(driver_id) {
        "" => "（可选）",
        v => v,
    }
}

pub(super) fn database_placeholder(driver_id: &str) -> &'static str {
    match driver_id {
        "redis" => "0",
        "sqlite" => "SQLite 不使用数据库名",
        "mongodb" => "如：mydb",
        "postgres" => "如：postgres / mydb",
        _ => "如：mydb",
    }
}

pub(super) fn uri_placeholder(driver_id: &str) -> &'static str {
    match driver_id {
        "sqlite" => "sqlite:///path/to/database.db",
        "postgres" => "postgres://user:pass@host:5432/db?sslmode=require",
        "redis" => "redis://user:pass@host:6379/0（rediss:// 为 TLS）",
        "mongodb" => "mongodb://user:pass@host:27017/db?tls=true",
        _ => "mysql://user:pass@host:3306/db",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_defaults_match_driver() {
        assert_eq!(default_port("mysql"), 3306);
        assert_eq!(default_port("postgres"), 5432);
        assert_eq!(default_port("redis"), 6379);
        assert_eq!(default_port("mongodb"), 27017);
    }

    #[test]
    fn username_defaults_only_for_sql_drivers() {
        assert_eq!(default_username("mysql"), "root");
        assert_eq!(default_username("postgres"), "postgres");
        assert_eq!(default_username("redis"), "");
        assert_eq!(default_username("mongodb"), "");
        assert_eq!(username_placeholder("postgres"), "postgres");
        assert_eq!(username_placeholder("redis"), "（可选）");
    }

    #[test]
    fn uri_placeholder_matches_scheme() {
        assert!(uri_placeholder("mysql").starts_with("mysql://"));
        assert!(uri_placeholder("postgres").starts_with("postgres://"));
        assert!(uri_placeholder("redis").starts_with("redis://"));
        assert!(uri_placeholder("mongodb").starts_with("mongodb://"));
    }

    #[test]
    fn sqlite_uses_a_file_path_placeholder() {
        assert!(host_placeholder("sqlite").contains("app.db"));
        assert_eq!(host_placeholder("mysql"), DEFAULT_HOST);
    }

    #[test]
    fn database_placeholder_per_driver() {
        assert_eq!(database_placeholder("redis"), "0");
        assert_eq!(database_placeholder("mongodb"), "如：mydb");
    }
}
