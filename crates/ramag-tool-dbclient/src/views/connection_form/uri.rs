//! MongoDB 连接 URI 解析：`mongodb://[user[:pass]@]host[:port][/db][?k=v]` → 表单字段。
//! 纯函数便于测试；`mongodb+srv` 需 DNS SRV 查询，明确报不支持而非静默错连

/// URI 解析出的表单字段集
#[derive(Debug, PartialEq, Eq)]
pub(super) struct MongoUriParts {
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub password: String,
    pub database: Option<String>,
    pub auth_source: Option<String>,
    pub tls: bool,
}

/// 解析单主机 mongodb URI；多主机（副本集）与拓扑类参数显式报错，不静默降级
pub(super) fn parse_mongo_uri(raw: &str) -> Result<MongoUriParts, String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("mongodb+srv://") {
        let _ = rest;
        return Err("mongodb+srv 需 DNS SRV 解析，暂不支持；请改用标准 mongodb:// 地址".into());
    }
    let rest = raw
        .strip_prefix("mongodb://")
        .ok_or_else(|| "URI 须以 mongodb:// 开头".to_string())?;
    if rest.is_empty() {
        return Err("URI 缺少主机地址".into());
    }

    // 切出 query
    let (main, query) = match rest.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (rest, None),
    };
    // 切出 path（database）
    let (authority, database) = match main.split_once('/') {
        Some((a, d)) => (a, {
            let d = percent_decode(d);
            if d.is_empty() { None } else { Some(d) }
        }),
        None => (main, None),
    };
    // 切出 userinfo
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (username, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((name, pass)) => (percent_decode(name), percent_decode(pass)),
            None => (percent_decode(u), String::new()),
        },
        None => (String::new(), String::new()),
    };
    // 多主机（副本集）拒绝而非静默取首个：悄悄改变连接拓扑比报错更危险
    if hostport.contains(',') {
        return Err(
            "URI 含多个主机（副本集拓扑），本表单仅支持单主机直连；请填写要直连的那台节点".into(),
        );
    }
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p.parse().map_err(|_| format!("端口不是有效数字：{p}"))?;
            (h.to_string(), Some(port))
        }
        None => (hostport.to_string(), None),
    };
    if host.is_empty() {
        return Err("URI 缺少主机地址".into());
    }

    // query：识别 authSource / tls / ssl；影响连接拓扑或读写语义的参数显式拒绝
    // （静默忽略 replicaSet 等会让实际连接行为偏离 URI 声明）
    let mut auth_source = None;
    let mut tls = false;
    if let Some(q) = query {
        for pair in q.split('&') {
            let (k, v) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            match k.to_ascii_lowercase().as_str() {
                "authsource" => {
                    let v = percent_decode(v);
                    if !v.is_empty() {
                        auth_source = Some(v);
                    }
                }
                "tls" | "ssl" => tls = v.eq_ignore_ascii_case("true"),
                "replicaset" | "readpreference" | "directconnection" | "loadbalanced" => {
                    return Err(format!(
                        "URI 参数「{k}」影响连接拓扑，本表单不支持；请移除后重试（将单主机直连）"
                    ));
                }
                _ => {}
            }
        }
    }

    Ok(MongoUriParts {
        host,
        port,
        username,
        password,
        database,
        auth_source,
        tls,
    })
}

/// 最简 percent 解码（%XX → 字节）；URI 中用户名/密码常见转义（@ : / 等）
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 1
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_uri_parsed() {
        let p = parse_mongo_uri(
            "mongodb://alice:p%40ss@db.example.com:27018/orders?authSource=admin&tls=true",
        )
        .unwrap();
        assert_eq!(p.host, "db.example.com");
        assert_eq!(p.port, Some(27018));
        assert_eq!(p.username, "alice");
        assert_eq!(p.password, "p@ss"); // %40 → @
        assert_eq!(p.database.as_deref(), Some("orders"));
        assert_eq!(p.auth_source.as_deref(), Some("admin"));
        assert!(p.tls);
    }

    #[test]
    fn minimal_uri_parsed() {
        let p = parse_mongo_uri("mongodb://localhost").unwrap();
        assert_eq!(p.host, "localhost");
        assert_eq!(p.port, None);
        assert!(p.username.is_empty());
        assert_eq!(p.database, None);
        assert!(!p.tls);
    }

    #[test]
    fn replica_set_rejected_explicitly() {
        // 多主机与拓扑参数都不允许静默降级——必须显式报错
        let e =
            parse_mongo_uri("mongodb://a.example.com:27017,b.example.com:27018/db").unwrap_err();
        assert!(e.contains("多个主机"));
        let e2 = parse_mongo_uri("mongodb://h:27017/db?replicaSet=rs0").unwrap_err();
        assert!(e2.contains("replicaSet") || e2.contains("replicaset"));
    }

    #[test]
    fn srv_rejected_with_hint() {
        let e = parse_mongo_uri("mongodb+srv://cluster0.example.mongodb.net/db").unwrap_err();
        assert!(e.contains("srv"));
    }

    #[test]
    fn bad_scheme_and_port_rejected() {
        assert!(parse_mongo_uri("mysql://x").is_err());
        assert!(parse_mongo_uri("mongodb://host:notaport").is_err());
        assert!(parse_mongo_uri("mongodb://").is_err());
    }
}
