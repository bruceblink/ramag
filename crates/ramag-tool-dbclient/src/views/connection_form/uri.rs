//! 连接 URI 解析。

use ramag_domain::entities::{ConnectionConfig, DriverKind};

/// URI 粘贴上限，包含转义余量。
pub(super) const MAX_URI_BYTES: usize = 256 * 1024;

/// URI 字段。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct UriParts {
    /// URI scheme 对应的驱动。
    pub driver_id: &'static str,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub password: String,
    pub database: Option<String>,
    /// MongoDB authSource。
    pub auth_source: Option<String>,
    pub tls: bool,
}

/// 回填 URI，不包含密码。
pub(super) fn connection_uri_without_password(config: &ConnectionConfig) -> String {
    let scheme = match (config.driver, config.tls) {
        (DriverKind::Redis, true) => "rediss",
        (DriverKind::Mysql, _) => "mysql",
        (DriverKind::Postgres, _) => "postgres",
        (DriverKind::Redis, false) => "redis",
        (DriverKind::Mongodb, _) => "mongodb",
    };
    let mut uri = format!("{scheme}://");
    if !config.username.is_empty() {
        uri.push_str(&percent_encode(&config.username));
        uri.push('@');
    }
    if config.host.contains(':') && !config.host.starts_with('[') {
        uri.push('[');
        uri.push_str(&config.host);
        uri.push(']');
    } else {
        uri.push_str(&config.host);
    }
    uri.push(':');
    uri.push_str(&config.port.to_string());
    if let Some(database) = config.database.as_deref()
        && !database.is_empty()
    {
        uri.push('/');
        uri.push_str(&percent_encode(database));
    }
    let mut query = Vec::new();
    if config.tls && config.driver != DriverKind::Redis {
        query.push("tls=true".to_string());
    }
    if config.driver == DriverKind::Mongodb
        && let Some(auth_source) = config.auth_source.as_deref()
        && !auth_source.is_empty()
    {
        query.push(format!("authSource={}", percent_encode(auth_source)));
    }
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(&query.join("&"));
    }
    uri
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// 解析单主机 URI，拒绝不支持的拓扑参数。
pub(super) fn parse_connection_uri(raw: &str) -> Result<UriParts, String> {
    if raw.len() > MAX_URI_BYTES {
        return Err(format!(
            "URI 过长：{} bytes，最多 {MAX_URI_BYTES} bytes",
            raw.len()
        ));
    }
    let raw = raw.trim();
    if raw.starts_with("mongodb+srv://") {
        return Err("mongodb+srv 需 DNS SRV 解析，暂不支持；请改用标准 mongodb:// 地址".into());
    }
    // scheme 决定驱动与默认 TLS。
    let (driver_id, rest, tls_from_scheme) = [
        ("mysql", "mysql://", false),
        ("postgres", "postgres://", false),
        ("postgres", "postgresql://", false),
        ("redis", "redis://", false),
        ("redis", "rediss://", true),
        ("mongodb", "mongodb://", false),
    ]
    .iter()
    .find_map(|(driver, scheme, tls)| raw.strip_prefix(scheme).map(|rest| (*driver, rest, *tls)))
    .ok_or_else(|| {
        "URI 须以 mysql:// / postgres:// / redis://（rediss://）/ mongodb:// 开头".to_string()
    })?;
    if rest.is_empty() {
        return Err("URI 缺少主机地址".into());
    }

    let (main, query) = match rest.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (rest, None),
    };
    let (authority, database) = match main.split_once('/') {
        Some((a, d)) => (a, {
            let d = percent_decode(d)?;
            if d.is_empty() { None } else { Some(d) }
        }),
        None => (main, None),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (username, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((name, pass)) => (percent_decode(name)?, percent_decode(pass)?),
            None => (percent_decode(u)?, String::new()),
        },
        None => (String::new(), String::new()),
    };
    // 拒绝多主机 URI，避免改变拓扑。
    if hostport.contains(',') {
        return Err(
            "URI 含多个主机（副本集拓扑），本表单仅支持单主机直连；请填写要直连的那台节点".into(),
        );
    }
    let (host, port) = parse_host_port(hostport)?;
    if host.is_empty() {
        return Err("URI 缺少主机地址".into());
    }
    // 提前校验 Redis 库号。
    if driver_id == "redis"
        && let Some(db) = &database
        && db.parse::<u8>().is_err()
    {
        return Err(format!("Redis URI 的库号须为 0 - 255 的数字：{db}"));
    }

    // 解析 TLS、authSource，并拒绝拓扑参数。
    let mut auth_source = None;
    let mut tls = tls_from_scheme;
    if let Some(q) = query {
        for pair in q.split('&') {
            let (k, v) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            match k.to_ascii_lowercase().as_str() {
                "tls" | "ssl" => {
                    tls = match v.to_ascii_lowercase().as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("URI 参数「{k}」须为 true 或 false")),
                    }
                }
                // SQL sslmode 映射为 TLS 开关。
                "sslmode" | "ssl-mode" if matches!(driver_id, "mysql" | "postgres") => {
                    tls = parse_ssl_mode(v)?;
                }
                "authsource" if driver_id == "mongodb" => {
                    let v = percent_decode(v)?;
                    if !v.is_empty() {
                        auth_source = Some(v);
                    }
                }
                "replicaset" | "readpreference" | "directconnection" | "loadbalanced"
                    if driver_id == "mongodb" =>
                {
                    return Err(format!(
                        "URI 参数「{k}」影响连接拓扑，本表单不支持；请移除后重试（将单主机直连）"
                    ));
                }
                _ => {}
            }
        }
    }

    Ok(UriParts {
        driver_id,
        host,
        port,
        username,
        password,
        database,
        auth_source,
        tls,
    })
}

/// 将 SQL sslmode 映射为 TLS 开关。
fn parse_ssl_mode(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().replace('_', "-").as_str() {
        "disable" | "disabled" | "allow" | "prefer" | "preferred" => Ok(false),
        "require" | "required" | "verify-ca" | "verify-full" | "verify-identity" => Ok(true),
        other => Err(format!("URI 参数 sslmode 值不支持：{other}")),
    }
}

fn parse_host_port(hostport: &str) -> Result<(String, Option<u16>), String> {
    if let Some(bracketed) = hostport.strip_prefix('[') {
        let close = bracketed
            .find(']')
            .ok_or_else(|| "IPv6 主机缺少右方括号 ]".to_string())?;
        let host = &bracketed[..close];
        if host.is_empty() {
            return Err("URI 缺少主机地址".into());
        }
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            None
        } else if let Some(port) = suffix.strip_prefix(':') {
            Some(parse_port(port)?)
        } else {
            return Err("IPv6 主机右方括号后只能跟 :port".into());
        };
        return Ok((host.to_string(), port));
    }

    if hostport.matches(':').count() > 1 {
        return Err("IPv6 地址须使用方括号，例如 mongodb://[::1]:27017".into());
    }
    match hostport.rsplit_once(':') {
        Some((host, port)) => Ok((host.to_string(), Some(parse_port(port)?))),
        None => Ok((hostport.to_string(), None)),
    }
}

fn parse_port(raw: &str) -> Result<u16, String> {
    let port = raw
        .parse::<u16>()
        .map_err(|_| format!("端口不是有效数字：{raw}"))?;
    if port == 0 {
        return Err("端口须为 1 - 65535".into());
    }
    Ok(port)
}

/// 最简百分号解码。
fn percent_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let encoded = bytes
                .get(i + 1..i + 3)
                .ok_or_else(|| format!("百分号编码不完整：{}", &s[i..]))?;
            let high = hex_value(encoded[0]).ok_or_else(|| {
                format!(
                    "百分号编码无效：%{}{}",
                    encoded[0] as char, encoded[1] as char
                )
            })?;
            let low = hex_value(encoded[1]).ok_or_else(|| {
                format!(
                    "百分号编码无效：%{}{}",
                    encoded[0] as char, encoded[1] as char
                )
            })?;
            out.push((high << 4) | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|error| format!("百分号解码结果不是有效 UTF-8：{error}"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_mongo_uri_parsed() {
        let p = parse_connection_uri(
            "mongodb://alice:p%40ss@db.example.com:27018/orders?authSource=admin&tls=true",
        )
        .unwrap();
        assert_eq!(p.driver_id, "mongodb");
        assert_eq!(p.host, "db.example.com");
        assert_eq!(p.port, Some(27018));
        assert_eq!(p.username, "alice");
        assert_eq!(p.password, "p@ss");
        assert_eq!(p.database.as_deref(), Some("orders"));
        assert_eq!(p.auth_source.as_deref(), Some("admin"));
        assert!(p.tls);
    }

    #[test]
    fn minimal_uri_parsed() {
        let p = parse_connection_uri("mongodb://localhost").unwrap();
        assert_eq!(p.host, "localhost");
        assert_eq!(p.port, None);
        assert!(p.username.is_empty());
        assert_eq!(p.database, None);
        assert!(!p.tls);
    }

    #[test]
    fn mysql_uri_with_ssl_mode() {
        let p = parse_connection_uri("mysql://root:secret@10.0.0.8:3307/app?ssl-mode=REQUIRED")
            .unwrap();
        assert_eq!(p.driver_id, "mysql");
        assert_eq!(p.host, "10.0.0.8");
        assert_eq!(p.port, Some(3307));
        assert_eq!(p.username, "root");
        assert_eq!(p.password, "secret");
        assert_eq!(p.database.as_deref(), Some("app"));
        assert_eq!(p.auth_source, None);
        assert!(p.tls);
    }

    #[test]
    fn postgres_uri_and_alias_scheme() {
        let p =
            parse_connection_uri("postgres://svc@pg.internal/warehouse?sslmode=disable").unwrap();
        assert_eq!(p.driver_id, "postgres");
        assert_eq!(p.username, "svc");
        assert_eq!(p.database.as_deref(), Some("warehouse"));
        assert!(!p.tls);

        let alias = parse_connection_uri("postgresql://pg.internal:5433/db").unwrap();
        assert_eq!(alias.driver_id, "postgres");
        assert_eq!(alias.port, Some(5433));

        assert!(parse_connection_uri("postgres://h/db?sslmode=maybe").is_err());
    }

    #[test]
    fn redis_uri_db_number_and_tls_scheme() {
        let p = parse_connection_uri("redis://:s3cret@cache.local:6380/2").unwrap();
        assert_eq!(p.driver_id, "redis");
        assert!(p.username.is_empty());
        assert_eq!(p.password, "s3cret");
        assert_eq!(p.database.as_deref(), Some("2"));
        assert!(!p.tls);

        let secure = parse_connection_uri("rediss://cache.local").unwrap();
        assert!(secure.tls);

        let e = parse_connection_uri("redis://cache.local/notdb").unwrap_err();
        assert!(e.contains("0 - 255"));
    }

    #[test]
    fn replica_set_rejected_explicitly() {
        // 多主机与拓扑参数都不允许静默降级——必须显式报错
        let e = parse_connection_uri("mongodb://a.example.com:27017,b.example.com:27018/db")
            .unwrap_err();
        assert!(e.contains("多个主机"));
        let e2 = parse_connection_uri("mongodb://h:27017/db?replicaSet=rs0").unwrap_err();
        assert!(e2.contains("replicaSet") || e2.contains("replicaset"));
    }

    #[test]
    fn srv_rejected_with_hint() {
        let e = parse_connection_uri("mongodb+srv://cluster0.example.mongodb.net/db").unwrap_err();
        assert!(e.contains("srv"));
    }

    #[test]
    fn bad_scheme_and_port_rejected() {
        assert!(parse_connection_uri("sqlite://x").is_err());
        assert!(parse_connection_uri("mongodb://host:notaport").is_err());
        assert!(parse_connection_uri("mongodb://host:0").is_err());
        assert!(parse_connection_uri("mongodb://").is_err());
    }

    #[test]
    fn ipv6_requires_brackets_and_is_unwrapped() {
        let parts = parse_connection_uri("mongodb://[::1]:27018/admin").unwrap();
        assert_eq!(parts.host, "::1");
        assert_eq!(parts.port, Some(27018));
        assert!(parse_connection_uri("mongodb://::1").is_err());
        assert!(parse_connection_uri("mongodb://[::1").is_err());
    }

    #[test]
    fn malformed_percent_encoding_and_tls_are_rejected() {
        assert!(parse_connection_uri("mongodb://user:%4@host").is_err());
        assert!(parse_connection_uri("mongodb://user:%GG@host").is_err());
        assert!(parse_connection_uri("mongodb://user:%FF@host").is_err());
        assert!(parse_connection_uri("mongodb://host/?tls=yes").is_err());
    }

    #[test]
    fn oversized_uri_is_rejected_before_parsing() {
        let raw = "x".repeat(MAX_URI_BYTES + 1);
        assert!(parse_connection_uri(&raw).is_err());
    }

    #[test]
    fn edit_uri_roundtrips_fields_without_exposing_password() {
        let mut config = ConnectionConfig::new_mongodb("mongo", "::1", 27018);
        config.username = "user@example".into();
        config.password = "visible-secret".into();
        config.database = Some("team data".into());
        config.auth_source = Some("admin/root".into());
        config.tls = true;

        let uri = connection_uri_without_password(&config);
        assert!(!uri.contains("visible-secret"));
        assert!(uri.contains("[::1]"));
        let parsed = parse_connection_uri(&uri).unwrap();
        assert_eq!(parsed.username, config.username);
        assert!(parsed.password.is_empty());
        assert_eq!(parsed.database, config.database);
        assert_eq!(parsed.auth_source, config.auth_source);
        assert!(parsed.tls);
    }
}
