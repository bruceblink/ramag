//! Redis key 空间元数据：SCAN 浏览的 (name, type, ttl) 载体

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, Result};

/// 当前界面只支持可安全显示的 UTF-8 Key；限制异常长 Key 避免树、搜索与命令参数放大内存。
pub const MAX_REDIS_KEY_BYTES: usize = 4 * 1024;
/// SCAN MATCH 来自即时搜索输入，保持与共享搜索框相同的资源边界。
pub const MAX_REDIS_MATCH_PATTERN_BYTES: usize = 4 * 1024;
/// 单个值 / 成员 / 字段参数上限；与 String 详情的最大可编辑前缀一致。
pub const MAX_REDIS_COMMAND_ARG_BYTES: usize = 4 * 1024 * 1024;
/// Redis 核心与模块命令名都很短，独立限制避免分类器为异常名称分配大写副本。
pub const MAX_REDIS_COMMAND_NAME_BYTES: usize = 256;
/// 单条命令全部参数的总字节上限，避免批量编辑形成超大网络缓冲区。
pub const MAX_REDIS_COMMAND_BYTES: usize = super::TRANSFER_BATCH_BYTES;
/// 单条命令参数个数上限，单独约束大量空参数造成的容器与协议开销。
pub const MAX_REDIS_COMMAND_ARGS: usize = 10_000;
/// Redis 界面单次最多保留的条目数；Key 树与集合详情共用，避免各处上限不一致。
pub const MAX_REDIS_LOADED_ITEMS: usize = 1_000_000;
/// 集合详情最多保留的元素数；同时受累计内容字节预算约束。
pub const MAX_REDIS_COLLECTION_ITEMS: usize = MAX_REDIS_LOADED_ITEMS;
/// Redis 值加载的全局字节上限；集合累计内容与单批响应共同复用。
pub const MAX_REDIS_COLLECTION_BYTES: usize = super::MAX_INTERACTIVE_RESULT_BYTES;
/// 单批 SCAN 的 COUNT 只是 hint，但仍需限制异常调用给服务端造成的瞬时压力。
pub const MAX_REDIS_SCAN_COUNT: u32 = 5_000;
/// `scan_all` 是小批辅助接口，不允许被直接调用成无界全库加载。
pub const MAX_REDIS_SCAN_ALL_KEYS: usize = 10_000;
/// 全量迁移首页预取窗口；限制同时驻留的值页数量，避免并发吞吐放大内存峰值。
pub const MAX_REDIS_VALUE_PAGE_BATCH: usize = 16;

pub fn validate_redis_key(key: &str) -> Result<()> {
    if key.len() > MAX_REDIS_KEY_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "Redis Key 超过 {} KiB 上限",
            MAX_REDIS_KEY_BYTES / 1024
        )));
    }
    if key.chars().any(char::is_control) {
        return Err(DomainError::InvalidConfig(
            "Redis Key 含当前界面无法安全显示的控制字符".into(),
        ));
    }
    Ok(())
}

pub fn validate_redis_match_pattern(pattern: &str) -> Result<()> {
    if pattern.len() > MAX_REDIS_MATCH_PATTERN_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "Redis MATCH 模式超过 {} KiB 上限",
            MAX_REDIS_MATCH_PATTERN_BYTES / 1024
        )));
    }
    if pattern.chars().any(char::is_control) {
        return Err(DomainError::InvalidConfig(
            "Redis MATCH 模式不能包含控制字符".into(),
        ));
    }
    Ok(())
}

pub fn validate_redis_command(argv: &[String]) -> Result<()> {
    validate_redis_command_parts(argv.iter().map(String::as_str))
}

fn validate_redis_command_parts<'a>(argv: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut count = 0usize;
    let mut total_bytes = 0usize;
    let mut command_seen = false;

    for argument in argv {
        count = count.saturating_add(1);
        if count > MAX_REDIS_COMMAND_ARGS {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 命令参数超过 {MAX_REDIS_COMMAND_ARGS} 个上限"
            )));
        }
        if argument.len() > MAX_REDIS_COMMAND_ARG_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 命令第 {count} 个参数超过 {} MiB 上限",
                MAX_REDIS_COMMAND_ARG_BYTES / 1024 / 1024
            )));
        }
        total_bytes = total_bytes
            .checked_add(argument.len())
            .ok_or_else(|| DomainError::InvalidConfig("Redis 命令参数总长度溢出".into()))?;
        if total_bytes > MAX_REDIS_COMMAND_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 命令超过 {} MiB 总字节上限",
                MAX_REDIS_COMMAND_BYTES / 1024 / 1024
            )));
        }

        if !command_seen {
            command_seen = true;
            if argument.is_empty() {
                return Err(DomainError::InvalidConfig(
                    "命令为空，至少需要命令名".into(),
                ));
            }
            if argument.len() > MAX_REDIS_COMMAND_NAME_BYTES
                || !argument.is_ascii()
                || argument
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
            {
                return Err(DomainError::InvalidConfig(
                    "Redis 命令名必须是至多 256 bytes、且不含空白或控制字符的 ASCII 文本".into(),
                ));
            }
        }
    }

    if command_seen {
        Ok(())
    } else {
        Err(DomainError::InvalidConfig(
            "命令为空，至少需要命令名".into(),
        ))
    }
}

pub fn validate_redis_collection_limit(limit: usize) -> Result<()> {
    if !(1..=MAX_REDIS_COLLECTION_ITEMS).contains(&limit) {
        return Err(DomainError::InvalidConfig(format!(
            "Redis 集合加载数量必须在 1 - {MAX_REDIS_COLLECTION_ITEMS} 之间"
        )));
    }
    Ok(())
}

pub fn validate_redis_scan_count(count: u32) -> Result<()> {
    if !(1..=MAX_REDIS_SCAN_COUNT).contains(&count) {
        return Err(DomainError::InvalidConfig(format!(
            "Redis SCAN COUNT 必须在 1 - {MAX_REDIS_SCAN_COUNT} 之间"
        )));
    }
    Ok(())
}

/// 与 `TYPE <key>` 应答对齐
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RedisType {
    String,
    List,
    Hash,
    Set,
    ZSet,
    Stream,
    /// key 不存在
    None,
}

impl RedisType {
    /// `TYPE` 应答 → 枚举。未知（模块自定义）映射为 None
    pub fn parse(s: &str) -> Self {
        match s {
            "string" => RedisType::String,
            "list" => RedisType::List,
            "hash" => RedisType::Hash,
            "set" => RedisType::Set,
            "zset" => RedisType::ZSet,
            "stream" => RedisType::Stream,
            _ => RedisType::None,
        }
    }

    /// `SCAN ... TYPE <type>` 用的小写字面量
    pub fn as_scan_arg(&self) -> &'static str {
        match self {
            RedisType::String => "string",
            RedisType::List => "list",
            RedisType::Hash => "hash",
            RedisType::Set => "set",
            RedisType::ZSet => "zset",
            RedisType::Stream => "stream",
            RedisType::None => "none",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            RedisType::String => "String",
            RedisType::List => "List",
            RedisType::Hash => "Hash",
            RedisType::Set => "Set",
            RedisType::ZSet => "ZSet",
            RedisType::Stream => "Stream",
            RedisType::None => "(none)",
        }
    }
}

/// Key 元数据。SCAN 阶段 `key_type` / `ttl_ms` 可为 None，UI 按需补查
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyMeta {
    /// utf-8 字符串，driver 保证（暂不支持二进制 key）
    pub key: String,
    /// None=未查询，Some(RedisType::None)=查过但 key 不存在
    pub key_type: Option<RedisType>,
    /// PTTL：None=未查询，-1=永久，-2=key 不存在，>=0=剩余毫秒
    pub ttl_ms: Option<i64>,
}

impl KeyMeta {
    /// 仅 key 名的最简元数据
    pub fn bare(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            key_type: None,
            ttl_ms: None,
        }
    }
}

/// SCAN 一批应答
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    /// 下次游标，0 = 遍历结束
    pub cursor: u64,
    pub keys: Vec<KeyMeta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_types() {
        assert_eq!(RedisType::parse("string"), RedisType::String);
        assert_eq!(RedisType::parse("zset"), RedisType::ZSet);
        assert_eq!(RedisType::parse("stream"), RedisType::Stream);
    }

    #[test]
    fn parse_unknown_falls_back_to_none() {
        assert_eq!(RedisType::parse("module"), RedisType::None);
        assert_eq!(RedisType::parse(""), RedisType::None);
    }

    #[test]
    fn scan_arg_roundtrip() {
        for t in [
            RedisType::String,
            RedisType::List,
            RedisType::Hash,
            RedisType::Set,
            RedisType::ZSet,
            RedisType::Stream,
        ] {
            assert_eq!(RedisType::parse(t.as_scan_arg()), t);
        }
    }

    #[test]
    fn redis_key_and_match_boundaries_are_explicit() {
        assert!(validate_redis_key(&"k".repeat(MAX_REDIS_KEY_BYTES)).is_ok());
        assert!(validate_redis_key(&"k".repeat(MAX_REDIS_KEY_BYTES + 1)).is_err());
        assert!(validate_redis_key("line\nkey").is_err());

        assert!(validate_redis_match_pattern(&"*".repeat(MAX_REDIS_MATCH_PATTERN_BYTES)).is_ok());
        assert!(
            validate_redis_match_pattern(&"*".repeat(MAX_REDIS_MATCH_PATTERN_BYTES + 1)).is_err()
        );
    }

    #[test]
    fn redis_command_bounds_arguments_count_and_total_bytes() {
        assert!(validate_redis_command(&["GET".into(), "key".into()]).is_ok());
        assert!(validate_redis_command(&[]).is_err());
        assert!(validate_redis_command(&["bad command".into()]).is_err());
        assert!(validate_redis_command(&["x".repeat(MAX_REDIS_COMMAND_NAME_BYTES + 1)]).is_err());
        assert!(validate_redis_command(&["GET".into(), "x\0y".into()]).is_ok());

        let max_argument = "v".repeat(MAX_REDIS_COMMAND_ARG_BYTES);
        assert!(validate_redis_command_parts(["SET", "key", max_argument.as_str()]).is_ok());
        let oversized_argument = "v".repeat(MAX_REDIS_COMMAND_ARG_BYTES + 1);
        assert!(validate_redis_command_parts(["SET", oversized_argument.as_str()]).is_err());

        // 7 个完整 4 MiB 参数 + 命令名 + 余量，恰好命中 32 MiB 总边界。
        let exact_remainder = "v".repeat(MAX_REDIS_COMMAND_ARG_BYTES - "SET".len());
        assert!(
            validate_redis_command_parts(
                std::iter::once("SET")
                    .chain(std::iter::repeat_n(max_argument.as_str(), 7))
                    .chain(std::iter::once(exact_remainder.as_str()))
            )
            .is_ok()
        );
        let over_remainder = "v".repeat(MAX_REDIS_COMMAND_ARG_BYTES - "SET".len() + 1);
        assert!(
            validate_redis_command_parts(
                std::iter::once("SET")
                    .chain(std::iter::repeat_n(max_argument.as_str(), 7))
                    .chain(std::iter::once(over_remainder.as_str()))
            )
            .is_err()
        );

        let empty = "";
        assert!(
            validate_redis_command_parts(
                std::iter::once("PING")
                    .chain(std::iter::repeat_n(empty, MAX_REDIS_COMMAND_ARGS - 1))
            )
            .is_ok()
        );
        assert!(
            validate_redis_command_parts(
                std::iter::once("PING").chain(std::iter::repeat_n(empty, MAX_REDIS_COMMAND_ARGS))
            )
            .is_err()
        );
    }

    #[test]
    fn redis_scan_and_collection_limits_reject_bypasses() {
        assert_eq!(MAX_REDIS_LOADED_ITEMS, 1_000_000);
        assert_eq!(MAX_REDIS_COLLECTION_ITEMS, MAX_REDIS_LOADED_ITEMS);
        assert_eq!(MAX_REDIS_COLLECTION_BYTES, 256 * 1024 * 1024);
        assert!(validate_redis_collection_limit(1).is_ok());
        assert!(validate_redis_collection_limit(MAX_REDIS_COLLECTION_ITEMS).is_ok());
        assert!(validate_redis_collection_limit(0).is_err());
        assert!(validate_redis_collection_limit(MAX_REDIS_COLLECTION_ITEMS + 1).is_err());

        assert!(validate_redis_scan_count(1).is_ok());
        assert!(validate_redis_scan_count(MAX_REDIS_SCAN_COUNT).is_ok());
        assert!(validate_redis_scan_count(0).is_err());
        assert!(validate_redis_scan_count(MAX_REDIS_SCAN_COUNT + 1).is_err());
    }
}
