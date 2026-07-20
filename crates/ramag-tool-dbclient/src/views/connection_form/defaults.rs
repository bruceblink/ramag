//! Driver 默认值表：新建连接时以 placeholder 虚影呈现，保存留空时回退到这些值

/// 默认 Host（所有 driver 一致）
pub(super) const DEFAULT_HOST: &str = "127.0.0.1";

/// driver 默认端口
pub(super) fn default_port(driver_id: &str) -> u16 {
    match driver_id {
        "postgres" => 5432,
        "redis" => 6379,
        "mongodb" => 27017,
        _ => 3306,
    }
}

/// driver 默认用户名；Redis / MongoDB 无默认（留空 = 无认证 / 无 ACL 用户）
pub(super) fn default_username(driver_id: &str) -> &'static str {
    match driver_id {
        "mysql" => "root",
        "postgres" => "postgres",
        _ => "",
    }
}

/// 用户名输入框虚影：有默认值显示默认值，无默认值提示可选
pub(super) fn username_placeholder(driver_id: &str) -> &'static str {
    match default_username(driver_id) {
        "" => "（可选）",
        v => v,
    }
}

/// 默认库输入框虚影：Redis 默认 0 号库 / MongoDB 默认 admin，SQL 类给示例
pub(super) fn database_placeholder(driver_id: &str) -> &'static str {
    match driver_id {
        "redis" => "0",
        "mongodb" => "如：mydb",
        "postgres" => "如：postgres / mydb",
        _ => "如：mydb",
    }
}

/// URI 粘贴框虚影：按 driver 显示对应 scheme 示例
pub(super) fn uri_placeholder(driver_id: &str) -> &'static str {
    match driver_id {
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
    fn database_placeholder_per_driver() {
        assert_eq!(database_placeholder("redis"), "0");
        assert_eq!(database_placeholder("mongodb"), "如：mydb");
    }
}
