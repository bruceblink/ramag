//! Resizable 面板尺寸持久化：订阅 `ResizablePanelEvent::Resized` 防抖落盘，
//! 创建后异步读回并经 `resize_panel` 恢复（上游注释明确该 API 供偏好持久化用）。
//! 各视图对自己的主分隔调用一次，pref_key 全局唯一

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::{Context, Entity, Subscription, Window, px};
use gpui_component::resizable::{ResizablePanelEvent, ResizableState};

/// 恢复前等待首帧建立 panels（insert_panel 在首次渲染时发生）
const RESTORE_DELAY: Duration = Duration::from_millis(150);
/// 拖动停顿后才落盘
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(600);
const MAX_PANEL_SIZES_PREF_BYTES: usize = 1024;

/// 给一个 Resizable 分隔挂「尺寸跨重启」：返回的订阅由调用方持有（随视图生命周期）。
/// 无 StorageGlobal（极早期调用）时仅返回空订阅语义的监听（不落盘不恢复）
pub fn persist_resizable_sizes<V: 'static>(
    state: &Entity<ResizableState>,
    pref_key: &'static str,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Subscription {
    // 布局可能按需出现（VCS 历史/详情默认隐藏）；保存待恢复值，首次 panel 数匹配时再应用。
    let pending_restore: Arc<parking_lot::Mutex<Option<Vec<f32>>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let restored_for_presence = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pending_for_observer = pending_restore.clone();
    let restored_for_observer = restored_for_presence.clone();
    cx.observe_in(state, window, move |_this, state, window, cx| {
        let Some(sizes) = pending_for_observer.lock().clone() else {
            return;
        };
        if state.read(cx).sizes().len() != sizes.len() || sizes.is_empty() {
            restored_for_observer.store(false, Ordering::Relaxed);
            return;
        }
        if restored_for_observer.swap(true, Ordering::Relaxed) {
            return;
        }
        // panel 数同步发生在渲染中；延后一帧，等真实 bounds / size_range 写回后再恢复。
        let pending = pending_for_observer.clone();
        let restored = restored_for_observer.clone();
        cx.defer_in(window, move |_this, window, cx| {
            let Some(sizes) = pending.lock().clone() else {
                restored.store(false, Ordering::Relaxed);
                return;
            };
            if state.read(cx).sizes().len() != sizes.len() || sizes.is_empty() {
                restored.store(false, Ordering::Relaxed);
                return;
            }
            state.update(cx, |state, cx| {
                for (index, size) in sizes.iter().enumerate() {
                    state.resize_panel(index, px(*size), window, cx);
                }
            });
        });
    })
    .detach();

    // 首帧建立面板后恢复尺寸。
    if let Some(storage) = crate::theme::storage_from_cx(cx) {
        let state_for_restore = state.clone();
        let pending_for_load = pending_restore.clone();
        let restored_for_load = restored_for_presence.clone();
        cx.spawn_in(window, async move |_, cx| {
            cx.background_executor().timer(RESTORE_DELAY).await;
            let json = match storage.get_preference(pref_key).await {
                Ok(Some(json)) => json,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e, pref_key, "load panel sizes failed");
                    return;
                }
            };
            let sizes = match parse_sizes(&json) {
                Ok(sizes) => sizes,
                Err(e) => {
                    tracing::warn!(error = %e, pref_key, "ignore invalid panel sizes");
                    return;
                }
            };
            if restored_for_load.swap(true, Ordering::Relaxed) {
                return;
            }
            *pending_for_load.lock() = Some(sizes.clone());
            let _ = cx.update(|window, cx| {
                if state_for_restore.read(cx).sizes().len() == sizes.len() && !sizes.is_empty() {
                    state_for_restore.update(cx, |state, cx| {
                        for (index, size) in sizes.iter().enumerate() {
                            // 上游 API 会按各 panel 的 size_range 和容器尺寸二次 clamp。
                            state.resize_panel(index, px(*size), window, cx);
                        }
                    });
                } else {
                    restored_for_load.store(false, Ordering::Relaxed);
                }
            });
        })
        .detach();
    }

    // 拖动事件按代次防抖后落盘。
    let generation = Arc::new(AtomicU64::new(0));
    let write_lock = Arc::new(futures::lock::Mutex::new(()));
    let pending_for_persist = pending_restore.clone();
    let restored_for_persist = restored_for_presence;
    cx.subscribe_in(
        state,
        window,
        move |_this, state, _e: &ResizablePanelEvent, window, cx| {
            let Some(storage) = crate::theme::storage_from_cx(cx) else {
                return;
            };
            let my_gen = generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            let generation = generation.clone();
            let write_lock = write_lock.clone();
            let sizes: Vec<f32> = state
                .read(cx)
                .sizes()
                .iter()
                .map(|p| f32::from(*p))
                .collect();
            *pending_for_persist.lock() = Some(sizes.clone());
            restored_for_persist.store(true, Ordering::Relaxed);
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(PERSIST_DEBOUNCE).await;
                    if generation.load(Ordering::Relaxed) != my_gen {
                        return;
                    }
                    let Ok(json) = serde_json::to_string(&sizes) else {
                        return;
                    };
                    let _guard = write_lock.lock().await;
                    if generation.load(Ordering::Relaxed) != my_gen {
                        return;
                    }
                    if let Err(e) = storage.set_preference(pref_key, &json).await {
                        tracing::warn!(error = %e, pref_key, "persist panel sizes failed");
                    }
                })
                .detach();
        },
    )
}

/// 偏好属于外部输入：拒绝 NaN/无穷和非正尺寸，避免布局恢复成不可交互状态。
fn parse_sizes(json: &str) -> Result<Vec<f32>, String> {
    if json.len() > MAX_PANEL_SIZES_PREF_BYTES {
        return Err(format!("面板尺寸偏好过大：{} bytes", json.len()));
    }
    let sizes: Vec<f32> =
        serde_json::from_str(json).map_err(|e| format!("尺寸数据格式无效：{e}"))?;
    if sizes.len() > 8 {
        return Err(format!("面板数量异常：{}", sizes.len()));
    }
    if sizes.iter().any(|size| !size.is_finite() || *size <= 0.0) {
        return Err("面板尺寸须为有限正数".into());
    }
    Ok(sizes)
}

#[cfg(test)]
mod tests {
    use super::{MAX_PANEL_SIZES_PREF_BYTES, parse_sizes};

    #[test]
    fn parses_valid_sizes() {
        assert_eq!(parse_sizes("[220.0,640.0]"), Ok(vec![220.0, 640.0]));
    }

    #[test]
    fn rejects_non_positive_sizes() {
        assert!(parse_sizes("[0.0,640.0]").is_err());
        assert!(parse_sizes(&" ".repeat(MAX_PANEL_SIZES_PREF_BYTES + 1)).is_err());
    }
}
