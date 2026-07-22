//! 跨工具的轻量偏好。状态放在 App Global 中，修改后所有已打开视图立即读取同一值。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::lock::Mutex;
use gpui::{App, Global};
use parking_lot::RwLock;
use ramag_domain::traits::Storage;

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

/// 串行写入并丢弃同 key 的过期任务，保证用户快速连续操作后“最后一次选择”最终落盘。
pub fn persist_preference_latest(key: &'static str, value: String, cx: &mut App) {
    let Some(storage) = crate::theme::storage_from_cx(cx) else {
        return;
    };
    persist_preference_latest_with_storage(key, value, storage, cx);
}

/// 使用调用方持有的存储执行“同 key 仅最新值落盘”；适合本身已注入 Storage 的工具视图。
pub fn persist_preference_latest_with_storage(
    key: &'static str,
    value: String,
    storage: Arc<dyn Storage>,
    cx: &mut App,
) {
    let writer = if let Some(global) = cx.try_global::<PreferenceWriterGlobal>() {
        global.0.clone()
    } else {
        let writer = PreferenceWriter::default();
        cx.set_global(PreferenceWriterGlobal(writer.clone()));
        writer
    };
    let revision = writer
        .next_revision
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    writer.latest_by_key.write().insert(key, revision);

    cx.background_executor()
        .spawn(async move {
            let _guard = writer.lock.lock().await;
            if writer.latest_by_key.read().get(key).copied() != Some(revision) {
                return;
            }
            if let Err(error) = storage.set_preference(key, &value).await {
                tracing::warn!(error = %error, preference = key, "persist preference failed");
            }
        })
        .detach();
}
