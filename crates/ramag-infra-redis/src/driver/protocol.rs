//! Redis 协议应答解析与资源预算。

use ramag_domain::entities::{KeyMeta, MAX_REDIS_COLLECTION_BYTES, ScanResult, validate_redis_key};
use ramag_domain::error::{DomainError, Result};
use redis::Value as RV;

pub(super) const MAX_RESPONSE_BYTES: usize = MAX_REDIS_COLLECTION_BYTES;
const MAX_RESPONSE_NODES: usize = 100_000;
const MAX_RESPONSE_DEPTH: usize = 64;

/// 解析 SCAN 系列应答中的游标与载荷。
pub(crate) fn scan_parts(v: RV, cmd: &str) -> Result<(u64, RV)> {
    match v {
        RV::Array(mut values) if values.len() == 2 => {
            let payload = values.pop().unwrap_or(RV::Nil);
            let cursor = parse_cursor(values.pop().unwrap_or(RV::Nil), cmd)?;
            Ok((cursor, payload))
        }
        RV::Nil => Ok((0, RV::Nil)),
        other => Err(DomainError::QueryFailed(format!(
            "{cmd} 应答格式异常：{other:?}"
        ))),
    }
}

pub(super) fn parse_cursor(value: RV, cmd: &str) -> Result<u64> {
    let text = match value {
        RV::BulkString(bytes) => String::from_utf8(bytes)
            .map_err(|error| DomainError::QueryFailed(format!("{cmd} cursor 非 UTF-8：{error}")))?,
        RV::SimpleString(text) => text,
        RV::Int(value) if value >= 0 => return Ok(value as u64),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "{cmd} cursor 类型异常：{other:?}"
            )));
        }
    };
    text.parse::<u64>()
        .map_err(|error| DomainError::QueryFailed(format!("{cmd} cursor 非数字：{error}")))
}

pub(super) fn parse_scan_response(value: RV) -> Result<ScanResult> {
    ensure_response_budget(&value, "SCAN")?;
    let (cursor, keys_raw) = scan_parts(value, "SCAN")?;
    let keys_raw = match keys_raw {
        RV::Array(values) => values,
        other => {
            return Err(DomainError::QueryFailed(format!(
                "SCAN keys 非数组：{other:?}"
            )));
        }
    };
    let keys = keys_raw
        .into_iter()
        .map(|value| {
            let key = match value {
                RV::BulkString(bytes) => String::from_utf8(bytes).map_err(|error| {
                    DomainError::QueryFailed(format!(
                        "SCAN 返回了非 UTF-8 键，当前版本无法安全操作该键：{error}"
                    ))
                })?,
                RV::SimpleString(key) => key,
                other => {
                    return Err(DomainError::QueryFailed(format!(
                        "SCAN 键类型异常：{other:?}"
                    )));
                }
            };
            validate_redis_key(&key).map_err(|error| {
                DomainError::QueryFailed(format!("SCAN 返回了当前版本无法安全操作的键：{error}"))
            })?;
            Ok(KeyMeta::bare(key))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ScanResult { cursor, keys })
}

#[derive(Clone, Copy)]
pub(super) struct ResponseLimits {
    pub(super) bytes: usize,
    pub(super) nodes: usize,
    pub(super) depth: usize,
}

struct ResponseBudget {
    limits: ResponseLimits,
    bytes: usize,
    nodes: usize,
}

impl ResponseBudget {
    fn visit(&mut self, value: &RV, depth: usize) -> bool {
        if depth > self.limits.depth || self.nodes >= self.limits.nodes {
            return false;
        }
        self.nodes += 1;
        match value {
            RV::BulkString(bytes) => self.add_bytes(bytes.len()),
            RV::SimpleString(text) | RV::VerbatimString { text, .. } => self.add_bytes(text.len()),
            RV::Array(values) | RV::Set(values) | RV::Push { data: values, .. } => values
                .iter()
                .all(|value| self.visit(value, depth.saturating_add(1))),
            RV::Map(pairs) => pairs.iter().all(|(key, value)| {
                self.visit(key, depth.saturating_add(1))
                    && self.visit(value, depth.saturating_add(1))
            }),
            RV::Attribute { data, attributes } => {
                self.visit(data, depth.saturating_add(1))
                    && attributes.iter().all(|(key, value)| {
                        self.visit(key, depth.saturating_add(1))
                            && self.visit(value, depth.saturating_add(1))
                    })
            }
            RV::BigNumber(number) => {
                self.add_bytes(usize::try_from(number.bits()).unwrap_or(usize::MAX))
            }
            RV::Nil
            | RV::Int(_)
            | RV::Okay
            | RV::Double(_)
            | RV::Boolean(_)
            | RV::ServerError(_) => true,
        }
    }

    fn add_bytes(&mut self, bytes: usize) -> bool {
        self.bytes = self.bytes.saturating_add(bytes);
        self.bytes <= self.limits.bytes
    }
}

pub(crate) fn ensure_response_budget(value: &RV, label: &str) -> Result<()> {
    ensure_response_with_limits(
        value,
        label,
        ResponseLimits {
            bytes: MAX_RESPONSE_BYTES,
            nodes: MAX_RESPONSE_NODES,
            depth: MAX_RESPONSE_DEPTH,
        },
    )
}

pub(super) fn ensure_response_with_limits(
    value: &RV,
    label: &str,
    limits: ResponseLimits,
) -> Result<()> {
    let mut budget = ResponseBudget {
        limits,
        bytes: 0,
        nodes: 0,
    };
    if budget.visit(value, 0) {
        Ok(())
    } else {
        Err(DomainError::QueryFailed(format!(
            "{label} 应答超过安全上限（{} MiB、{} 个节点或 {} 层嵌套），请缩小命令范围",
            limits.bytes / 1024 / 1024,
            limits.nodes,
            limits.depth
        )))
    }
}
