//! Redis 写命令识别：生产模式只读保护用。
//! 黑名单覆盖全部主流写命令；带可选写参数的命令（SORT / EVAL / GEORADIUS / BITFIELD / GETEX）
//! 保守归写，其 `_RO` 只读变体不在名单内，自动放行。
//! 管理类命令（CONFIG / CLUSTER / ACL / SCRIPT 等）整体归写——子命令不细分，
//! 少量只读子命令被一并拦截，但生产只读场景本就不应使用，符合保守封死原则

/// 命令名（大小写不敏感）是否为写命令。
/// 除固定黑名单外，还拦截：① 模块写命令（RedisJSON/TS/Search/Graph/Bloom 等，
/// 按 `点号 + 非只读后缀` 识别）；② 会把复用连接卡在特殊模式或危险的管理命令
pub fn is_write_command(cmd: &str) -> bool {
    let upper = cmd.to_ascii_uppercase();
    if WRITE_COMMANDS.contains(&upper.as_str()) || BLOCKING_OR_UNSAFE.contains(&upper.as_str()) {
        return true;
    }
    if upper.contains('.') {
        return is_module_write_command(&upper);
    }
    // 生产只读保护采用白名单：未知核心命令按写操作处理，避免新命令默认绕过保护。
    !READ_COMMANDS.contains(&upper.as_str())
}

const READ_COMMANDS: &[&str] = &[
    "BITCOUNT",
    "BITFIELD_RO",
    "BITPOS",
    "COMMAND",
    "DBSIZE",
    "DUMP",
    "ECHO",
    "EVALSHA_RO",
    "EVAL_RO",
    "EXISTS",
    "EXPIRETIME",
    "FCALL_RO",
    "GEODIST",
    "GEOHASH",
    "GEOPOS",
    "GEORADIUSBYMEMBER_RO",
    "GEORADIUS_RO",
    "GEOSEARCH",
    "GET",
    "GETBIT",
    "GETRANGE",
    "HEXISTS",
    "HGET",
    "HGETALL",
    "HKEYS",
    "HLEN",
    "HMGET",
    "HRANDFIELD",
    "HSCAN",
    "HSTRLEN",
    "HVALS",
    "INFO",
    "KEYS",
    "LASTSAVE",
    "LCS",
    "LINDEX",
    "LLEN",
    "LOLWUT",
    "LPOS",
    "LRANGE",
    "MGET",
    "OBJECT",
    "PEXPIRETIME",
    "PFCOUNT",
    "PING",
    "PTTL",
    "RANDOMKEY",
    "ROLE",
    "SCAN",
    "SCARD",
    "SDIFF",
    "SINTER",
    "SINTERCARD",
    "SISMEMBER",
    "SMEMBERS",
    "SMISMEMBER",
    "SORT_RO",
    "SRANDMEMBER",
    "STRLEN",
    "SUNION",
    "TIME",
    "TTL",
    "TYPE",
    "XINFO",
    "XLEN",
    "XPENDING",
    "XRANGE",
    "XREAD",
    "XREVRANGE",
    "ZCARD",
    "ZCOUNT",
    "ZDIFF",
    "ZINTER",
    "ZINTERCARD",
    "ZLEXCOUNT",
    "ZMSCORE",
    "ZRANDMEMBER",
    "ZRANGE",
    "ZRANGEBYLEX",
    "ZRANGEBYSCORE",
    "ZRANK",
    "ZREVRANGE",
    "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE",
    "ZREVRANK",
    "ZSCORE",
    "ZUNION",
];

/// 模块命令形如 `JSON.SET` / `TS.ADD` / `FT.DROPINDEX`。无法穷举各模块的读命令，
/// 故采用保守策略：已知只读后缀（GET/MGET/RANGE/INFO 等）放行，其余点号命令一律当写。
fn is_module_write_command(upper: &str) -> bool {
    let Some((namespace, sub)) = upper.split_once('.') else {
        return false;
    };
    // 未知模块命名空间同样保守拦截。
    const MODULE_NAMESPACES: &[&str] = &[
        "JSON", "TS", "FT", "GRAPH", "BF", "CF", "CMS", "TOPK", "TDIGEST", "SEARCH",
    ];
    if !MODULE_NAMESPACES.contains(&namespace) {
        return true;
    }
    // 只读子命令后缀白名单：命中放行，其余（SET/DEL/ADD/INCRBY/CREATE/DROP…）当写
    const READ_SUFFIXES: &[&str] = &[
        "GET",
        "MGET",
        "TYPE",
        "STRLEN",
        "ARRLEN",
        "OBJLEN",
        "OBJKEYS",
        "RESP",
        "DEBUG",
        "RANGE",
        "REVRANGE",
        "MRANGE",
        "MREVRANGE",
        "GET",
        "INFO",
        "QUERYINDEX",
        "SEARCH",
        "AGGREGATE",
        "EXPLAIN",
        "PROFILE",
        "CARD",
        "COUNT",
        "QUERY",
        "EXISTS",
        "MEXISTS",
        "RANK",
        "MIN",
        "MAX",
        "LIST",
        "SUGGET",
    ];
    !READ_SUFFIXES.contains(&sub)
}

const WRITE_COMMANDS: &[&str] = &[
    // String / 通用
    "SET",
    "SETNX",
    "SETEX",
    "PSETEX",
    "SETRANGE",
    "APPEND",
    "GETSET",
    "GETDEL",
    "GETEX",
    "MSET",
    "MSETNX",
    "INCR",
    "DECR",
    "INCRBY",
    "DECRBY",
    "INCRBYFLOAT",
    "DEL",
    "UNLINK",
    "EXPIRE",
    "PEXPIRE",
    "EXPIREAT",
    "PEXPIREAT",
    "PERSIST",
    "RENAME",
    "RENAMENX",
    "MOVE",
    "COPY",
    "RESTORE",
    "MIGRATE",
    // 列表
    "LPUSH",
    "RPUSH",
    "LPUSHX",
    "RPUSHX",
    "LPOP",
    "RPOP",
    "LSET",
    "LINSERT",
    "LREM",
    "LTRIM",
    "RPOPLPUSH",
    "LMOVE",
    "BLPOP",
    "BRPOP",
    "BLMOVE",
    "BRPOPLPUSH",
    "LMPOP",
    "BLMPOP",
    // 集合
    "SADD",
    "SREM",
    "SPOP",
    "SMOVE",
    "SINTERSTORE",
    "SUNIONSTORE",
    "SDIFFSTORE",
    // 哈希
    "HSET",
    "HSETNX",
    "HMSET",
    "HDEL",
    "HINCRBY",
    "HINCRBYFLOAT",
    "HEXPIRE",
    "HPEXPIRE",
    "HEXPIREAT",
    "HPEXPIREAT",
    "HPERSIST",
    // 有序集合
    "ZADD",
    "ZREM",
    "ZINCRBY",
    "ZPOPMIN",
    "ZPOPMAX",
    "BZPOPMIN",
    "BZPOPMAX",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREMRANGEBYLEX",
    "ZRANGESTORE",
    "ZDIFFSTORE",
    "ZINTERSTORE",
    "ZUNIONSTORE",
    "ZMPOP",
    "BZMPOP",
    // 流
    "XADD",
    "XDEL",
    "XTRIM",
    "XSETID",
    "XGROUP",
    "XCLAIM",
    "XAUTOCLAIM",
    "XACK",
    "XREADGROUP",
    // HyperLogLog 基数统计
    "PFADD",
    "PFMERGE",
    // Geo（带 STORE 写，保守归写）
    "GEOADD",
    "GEOSEARCHSTORE",
    "GEORADIUS",
    "GEORADIUSBYMEMBER",
    // 位图
    "SETBIT",
    "BITOP",
    "BITFIELD",
    // Scripting（可能写，保守归写；_RO 变体放行）
    "EVAL",
    "EVALSHA",
    "FCALL",
    // 排序（带 STORE 写；SORT_RO 放行）
    "SORT",
    // Server / 管理（改状态 / 危险）
    "FLUSHDB",
    "FLUSHALL",
    "SWAPDB",
    "CONFIG",
    "FUNCTION",
    "SCRIPT",
    "DEBUG",
    "SHUTDOWN",
    "SAVE",
    "BGSAVE",
    "BGREWRITEAOF",
    "SLAVEOF",
    "REPLICAOF",
    "FAILOVER",
    "RESET",
    "ACL",
    "CLUSTER",
    "LATENCY",
];

/// 会把复用的 ConnectionManager 卡在特殊模式或有危险副作用的命令：
/// MONITOR/SUBSCRIBE 类会让连接进入不可逆的接收模式，CLIENT KILL 可断别的连接。
/// 生产只读连接一律拦截（连非生产也不该经值编辑面板发这些）。
const BLOCKING_OR_UNSAFE: &[&str] = &[
    "MONITOR",
    "SUBSCRIBE",
    "PSUBSCRIBE",
    "SSUBSCRIBE",
    "UNSUBSCRIBE",
    "PUNSUBSCRIBE",
    "SUNSUBSCRIBE",
    "PUBLISH",
    "SPUBLISH",
    "CLIENT",
    "WAIT",
    "AUTH",
    "HELLO",
    "QUIT",
    "READONLY",
    "READWRITE",
    "SELECT",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_detected() {
        for c in [
            "SET", "del", "HSET", "ZADD", "XADD", "FLUSHDB", "flushall", "expire", "rename",
            "lpush", "getex", "sort", "eval", "config",
        ] {
            assert!(is_write_command(c), "{c} 应判为写命令");
        }
    }

    #[test]
    fn reads_allowed() {
        for c in [
            "GET",
            "mget",
            "HGETALL",
            "LRANGE",
            "SCAN",
            "TYPE",
            "TTL",
            "PTTL",
            "ZRANGE",
            "XRANGE",
            "INFO",
            "PING",
            "EXISTS",
            "SMEMBERS",
            "DBSIZE",
            "KEYS",
            "HSCAN",
            "PFCOUNT",
            // _RO 只读变体须放行
            "SORT_RO",
            "EVAL_RO",
            "EVALSHA_RO",
            "BITFIELD_RO",
            "GEORADIUS_RO",
        ] {
            assert!(!is_write_command(c), "{c} 应判为只读命令");
        }
    }

    #[test]
    fn module_write_commands_are_blocked() {
        // 模块写命令拦截
        for c in [
            "JSON.SET",
            "json.del",
            "TS.ADD",
            "TS.CREATE",
            "FT.DROPINDEX",
            "FT.CREATE",
            "GRAPH.DELETE",
            "BF.ADD",
            "CF.INSERT",
        ] {
            assert!(is_write_command(c), "{c} 应判为写命令");
        }
        // 模块只读命令放行
        for c in [
            "JSON.GET",
            "json.mget",
            "TS.RANGE",
            "TS.INFO",
            "FT.SEARCH",
            "FT.AGGREGATE",
            "BF.EXISTS",
        ] {
            assert!(!is_write_command(c), "{c} 应判为只读命令");
        }
        // 未知模块命令保守拦截，避免生产模式默认放行未来写命令。
        assert!(is_write_command("MY.CUSTOM"));
    }

    #[test]
    fn blocking_and_unsafe_commands_are_blocked() {
        for c in ["MONITOR", "subscribe", "CLIENT", "PUBLISH"] {
            assert!(is_write_command(c), "{c} 应被拦截");
        }
    }
}
