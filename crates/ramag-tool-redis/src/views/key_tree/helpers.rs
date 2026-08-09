//! Key 树操作的扫描、删除和展示辅助函数。

use ramag_app::RedisService;
use ramag_domain::entities::{ConnectionConfig, MAX_REDIS_SCAN_COUNT, RedisValue};
use ramag_domain::error::Result;

const SCAN_BATCH: usize = MAX_REDIS_SCAN_COUNT as usize;
const DEL_CHUNK: usize = 500;

pub(super) fn apply_local_rename(
    keys: &mut Vec<ramag_domain::entities::KeyMeta>,
    seen_keys: &mut std::collections::HashSet<String>,
    key_bytes: &mut usize,
    old: &str,
    new: &str,
) {
    // 服务端 RENAMENX 成功说明目标不存在；本地同名项只能是陈旧快照，应先移除。
    keys.retain(|meta| meta.key != new);
    if let Some(meta) = keys.iter_mut().find(|meta| meta.key == old) {
        meta.key = new.to_string();
    }
    seen_keys.clear();
    seen_keys.extend(keys.iter().map(|meta| meta.key.clone()));
    *key_bytes = keys
        .iter()
        .fold(0usize, |total, meta| total.saturating_add(meta.key.len()));
}

/// 循环「SCAN 收集一轮 → 分批 DEL」直到该 pattern 再无匹配；返回实际删除数。
/// 不一次性收集全部 key，内存上界 = SCAN_BATCH 个 key 名
pub(super) async fn delete_by_pattern(
    svc: &RedisService,
    config: &ConnectionConfig,
    db: u8,
    pattern: &str,
) -> Result<u64> {
    let mut total = 0u64;
    loop {
        let batch = svc
            .scan_all(config, db, Some(pattern), None, SCAN_BATCH)
            .await?;
        if batch.is_empty() {
            break;
        }
        let got = batch.len();
        for chunk in batch.chunks(DEL_CHUNK) {
            let mut argv = Vec::with_capacity(chunk.len() + 1);
            argv.push("DEL".to_string());
            argv.extend(chunk.iter().map(|k| k.key.clone()));
            let reply = svc.execute_command(config, db, argv).await?;
            total += parse_delete_count(reply)?;
        }
        // 单轮不足上限说明已扫到尾，无需再来一轮空扫
        if got < SCAN_BATCH {
            break;
        }
    }
    Ok(total)
}

fn parse_delete_count(reply: RedisValue) -> Result<u64> {
    match reply {
        RedisValue::Int(count) if count >= 0 => Ok(count as u64),
        RedisValue::Int(count) => Err(ramag_domain::error::DomainError::QueryFailed(format!(
            "DEL 返回无效负数：{count}"
        ))),
        other => Err(ramag_domain::error::DomainError::QueryFailed(format!(
            "DEL 返回了非整数应答：{other:?}"
        ))),
    }
}

pub(super) fn escape_glob(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub(super) fn truncate_label(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let mut head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        head.push('…');
    }
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_glob_specials() {
        assert_eq!(escape_glob("user"), "user");
        assert_eq!(escape_glob("a*b"), "a\\*b");
        assert_eq!(escape_glob("a?[c]"), "a\\?\\[c\\]");
        assert_eq!(escape_glob("a\\b"), "a\\\\b");
    }

    #[test]
    fn truncate_label_keeps_short_and_cuts_long() {
        assert_eq!(truncate_label("short", 10), "short");
        assert_eq!(truncate_label("数据库连接池", 3), "数据库…");
    }

    #[test]
    fn delete_count_rejects_unexpected_reply() {
        assert_eq!(parse_delete_count(RedisValue::Int(2)).ok(), Some(2));
        assert!(parse_delete_count(RedisValue::Int(-1)).is_err());
        assert!(parse_delete_count(RedisValue::Text("OK".into())).is_err());
    }

    #[test]
    fn local_rename_rebuilds_dedup_and_byte_accounting() {
        let mut keys = vec![
            ramag_domain::entities::KeyMeta::bare("old"),
            ramag_domain::entities::KeyMeta::bare("new"),
        ];
        let mut seen = std::collections::HashSet::from(["old".into(), "new".into()]);
        let mut bytes = 6;

        apply_local_rename(&mut keys, &mut seen, &mut bytes, "old", "new");

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "new");
        assert_eq!(seen, std::collections::HashSet::from(["new".into()]));
        assert_eq!(bytes, 3);
    }
}
