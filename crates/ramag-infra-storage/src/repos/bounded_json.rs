//! 对可能含大正文的 JSON 记录做流式序列化与输入长度校验，避免先构造无界 String。

use ramag_domain::error::{DomainError, Result};

struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    overflowed: bool,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(64 * 1024)),
            limit,
            overflowed: false,
        }
    }
}

impl std::io::Write for BoundedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.overflowed = true;
            return Err(std::io::Error::other("JSON record exceeds limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn serialize(
    value: &impl serde::Serialize,
    limit: usize,
    label: &str,
) -> Result<String> {
    let mut writer = BoundedBuffer::new(limit);
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.overflowed {
            return Err(DomainError::Storage(format!(
                "{label}序列化超过 {} MiB 安全上限",
                limit / 1024 / 1024
            )));
        }
        return Err(DomainError::Storage(format!("序列化{label}失败：{error}")));
    }
    String::from_utf8(writer.bytes)
        .map_err(|error| DomainError::Storage(format!("{label} JSON 不是 UTF-8：{error}")))
}

pub(crate) fn ensure_len(len: usize, limit: usize, label: &str) -> Result<()> {
    if len > limit {
        return Err(DomainError::Storage(format!(
            "{label}超过 {} MiB 安全上限",
            limit / 1024 / 1024
        )));
    }
    Ok(())
}

pub(crate) fn ensure_collection_budget(
    item_count: usize,
    total_bytes: usize,
    max_items: usize,
    max_bytes: usize,
    label: &str,
) -> Result<()> {
    if item_count > max_items {
        return Err(DomainError::Storage(format!(
            "{label}超过 {max_items} 条安全上限"
        )));
    }
    if total_bytes > max_bytes {
        return Err(DomainError::Storage(format!(
            "{label}总数据超过 {} MiB 安全上限",
            max_bytes / 1024 / 1024
        )));
    }
    Ok(())
}

pub(crate) fn next_collection_budget(
    item_count: usize,
    total_bytes: usize,
    item_bytes: usize,
    max_items: usize,
    max_bytes: usize,
    label: &str,
) -> Result<(usize, usize)> {
    let next_count = item_count
        .checked_add(1)
        .ok_or_else(|| DomainError::Storage(format!("{label}条数溢出")))?;
    let next_bytes = total_bytes
        .checked_add(item_bytes)
        .ok_or_else(|| DomainError::Storage(format!("{label}总数据大小溢出")))?;
    ensure_collection_budget(next_count, next_bytes, max_items, max_bytes, label)?;
    Ok((next_count, next_bytes))
}

#[cfg(test)]
mod tests {
    use super::{ensure_collection_budget, next_collection_budget, serialize};

    #[test]
    fn bounded_serialization_stops_before_large_allocation() {
        assert_eq!(serialize(&"a", 3, "测试记录").unwrap(), "\"a\"");
        assert!(serialize(&"abcd", 3, "测试记录").is_err());
    }

    #[test]
    fn collection_budget_accepts_boundary_and_rejects_overflow() {
        assert_eq!(
            next_collection_budget(1, 3, 2, 2, 5, "测试列表").unwrap(),
            (2, 5)
        );
        assert!(next_collection_budget(2, 5, 0, 2, 5, "测试列表").is_err());
        assert!(next_collection_budget(1, 5, 1, 2, 5, "测试列表").is_err());
        assert!(ensure_collection_budget(2, 5, 2, 5, "测试列表").is_ok());
    }
}
