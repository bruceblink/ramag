//! CLI 高危命令识别（命令 + 子命令粒度）：命中即弹确认（展示连接名 + DB）再执行。
//! 与 driver 层生产只读拦截互补——这里管的是「非生产连接上一 Enter 就毁库」的误操作

/// 返回该命令的危险说明（中文，用于确认弹窗）；None = 无需确认直接执行
pub(super) fn dangerous_reason(argv: &[String]) -> Option<&'static str> {
    let cmd = argv.first()?.to_ascii_uppercase();
    let sub = argv.get(1).map(|s| s.to_ascii_uppercase());
    match cmd.as_str() {
        "FLUSHDB" => Some("将清空当前 DB 的全部 key，不可恢复"),
        "FLUSHALL" => Some("将清空该实例所有 DB 的全部 key，不可恢复"),
        "SHUTDOWN" => Some("将关闭 Redis 服务进程，所有客户端断开"),
        "DEBUG" => Some("DEBUG 子命令可能崩溃 / 阻塞 / 重写数据，仅限排障使用"),
        "FAILOVER" => Some("将触发主从故障转移，影响整个集群拓扑"),
        "REPLICAOF" | "SLAVEOF" => Some("将改变主从复制拓扑，可能清空本机数据"),
        "SWAPDB" => Some("将交换两个 DB 的全部数据，正在使用它们的客户端立即受影响"),
        "RESET" => Some("将重置当前连接状态"),
        "MIGRATE" => Some("将把 key 迁移到其它实例（源端默认删除）"),
        "CONFIG" => match sub.as_deref() {
            // CONFIG GET 等读操作放行
            Some("SET") => Some("将在线修改服务器配置，立即生效"),
            Some("REWRITE") => Some("将把当前配置写回 redis.conf"),
            Some("RESETSTAT") => Some("将清零服务器统计信息"),
            _ => None,
        },
        "CLIENT" => match sub.as_deref() {
            Some("KILL") => Some("将强制断开其它客户端连接"),
            Some("PAUSE") => Some("将暂停所有客户端命令处理"),
            _ => None,
        },
        "SCRIPT" => match sub.as_deref() {
            Some("FLUSH") => Some("将清空全部已加载的 Lua 脚本缓存"),
            _ => None,
        },
        "FUNCTION" => match sub.as_deref() {
            Some("FLUSH") => Some("将删除全部已加载的 Function 库"),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::dangerous_reason;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn destructive_commands_flagged() {
        for cmd in [
            &["FLUSHALL"][..],
            &["flushdb"],
            &["SHUTDOWN", "NOSAVE"],
            &["debug", "sleep", "10"],
            &["CONFIG", "SET", "maxmemory", "0"],
            &["client", "kill", "id", "3"],
            &["SCRIPT", "FLUSH"],
            &["replicaof", "no", "one"],
        ] {
            assert!(dangerous_reason(&argv(cmd)).is_some(), "{cmd:?} 应需确认");
        }
    }

    #[test]
    fn read_and_common_commands_pass() {
        for cmd in [
            &["GET", "foo"][..],
            &["CONFIG", "GET", "maxmemory"],
            &["CLIENT", "LIST"],
            &["SCRIPT", "EXISTS", "abc"],
            &["SET", "k", "v"],
            &["DEL", "k"],
        ] {
            assert!(dangerous_reason(&argv(cmd)).is_none(), "{cmd:?} 不应拦截");
        }
    }
}
