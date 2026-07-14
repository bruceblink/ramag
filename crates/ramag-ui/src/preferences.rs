//! 跨工具的轻量偏好。状态放在 App Global 中，修改后所有已打开视图立即读取同一值。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::lock::Mutex;
use gpui::{App, Global};
use parking_lot::RwLock;

pub const SQL_AUTO_LIMIT_PREF: &str = "sql_auto_limit";
pub const SQL_AUTO_LIMIT_DEFAULT: usize = 10_000;
pub const SQL_AUTO_LIMIT_CHOICES: [usize; 3] = [1_000, 10_000, 50_000];

pub struct SqlAutoLimitGlobal(pub Option<usize>);
impl Global for SqlAutoLimitGlobal {}

#[derive(Clone)]
struct PreferenceWriter {
    next_revision: Arc<AtomicU64>,
    latest_by_key: Arc<RwLock<HashMap<&'static str, u64>>>,
    lock: Arc<Mutex<()>>,
}

impl Default for PreferenceWriter {
    fn default() -> Self {
        Self {
            next_revision: Arc::new(AtomicU64::new(0)),
            latest_by_key: Arc::new(RwLock::new(HashMap::new())),
            lock: Arc::new(Mutex::new(())),
        }
    }
}

struct PreferenceWriterGlobal(PreferenceWriter);
impl Global for PreferenceWriterGlobal {}

/// 解析落盘值。未知或损坏的值回到安全默认值；`off` 才明确表示关闭。
pub fn parse_sql_auto_limit(value: Option<&str>) -> Option<usize> {
    match value.map(str::trim) {
        Some("off" | "0") => None,
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|limit| SQL_AUTO_LIMIT_CHOICES.contains(limit))
            .or(Some(SQL_AUTO_LIMIT_DEFAULT)),
        None => Some(SQL_AUTO_LIMIT_DEFAULT),
    }
}

pub fn sql_auto_limit(cx: &App) -> Option<usize> {
    cx.try_global::<SqlAutoLimitGlobal>()
        .map(|value| value.0)
        .unwrap_or(Some(SQL_AUTO_LIMIT_DEFAULT))
}

/// 修改全局 SQL 自动限制并持久化。所有查询标签在渲染和执行时都直接读取该值。
pub fn set_sql_auto_limit(limit: Option<usize>, cx: &mut App) {
    if sql_auto_limit(cx) == limit {
        return;
    }
    cx.set_global(SqlAutoLimitGlobal(limit));
    cx.refresh_windows();

    let value = limit.map_or_else(|| "off".to_string(), |n| n.to_string());
    persist_preference_latest(SQL_AUTO_LIMIT_PREF, value, cx);
}

/// 串行写入并丢弃同 key 的过期任务，保证用户快速连续操作后“最后一次选择”最终落盘。
pub fn persist_preference_latest(key: &'static str, value: String, cx: &mut App) {
    let Some(storage) = crate::theme::storage_from_cx(cx) else {
        return;
    };
    let writer = if let Some(global) = cx.try_global::<PreferenceWriterGlobal>() {
        global.0.clone()
    } else {
        let writer = PreferenceWriter::default();
        cx.set_global(PreferenceWriterGlobal(writer.clone()));
        writer
    };
    let revision = writer.next_revision.fetch_add(1, Ordering::Relaxed) + 1;
    writer.latest_by_key.write().insert(key, revision);

    cx.background_executor()
        .spawn(async move {
            let _guard = writer.lock.lock().await;
            if writer.latest_by_key.read().get(key).copied() != Some(revision) {
                return;
            }
            if let Err(error) = storage.set_preference(key, &value).await {
                tracing::warn!(error = %error, preference = key, "failed to persist preference");
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_values() {
        assert_eq!(parse_sql_auto_limit(Some("1000")), Some(1_000));
        assert_eq!(parse_sql_auto_limit(Some("off")), None);
    }

    #[test]
    fn rejects_corrupted_or_unsupported_values() {
        assert_eq!(parse_sql_auto_limit(Some("abc")), Some(10_000));
        assert_eq!(parse_sql_auto_limit(Some("999999")), Some(10_000));
        assert_eq!(parse_sql_auto_limit(None), Some(10_000));
    }
}
