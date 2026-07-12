//! 增量 SCAN 状态机：分批加载 + 服务端 MATCH 下推 + 代际取消。
//! 每批回包做（代际, db, 连接）三重身份校验，换代 / 切库 / 切连接后在途批次一律作废

use gpui::Context;
use ramag_domain::entities::ConnectionConfig;
use tracing::{error, info};

use super::{KeyTreePanel, MAX_KEYS};

/// 单批 SCAN 的 COUNT hint
const SCAN_BATCH: u32 = 500;
/// 分批加载期间 Trie 节流重建阈值：较上次重建新增 key 数达到该值才重建一次
const REBUILD_STEP: usize = 2_000;

impl KeyTreePanel {
    /// 重新扫描：换代作废在途批次，清空后从 cursor=0 起按当前 MATCH 模式增量扫
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        self.scan_generation += 1;
        self.loading = true;
        self.error = None;
        self.keys.clear();
        self.tree.clear();
        self.last_rebuilt_count = 0;
        self.truncated = false;
        cx.notify();
        let generation = self.scan_generation;
        self.scan_next_batch(config, 0, generation, cx);
    }

    /// 停止扫描：换代终止续扫，保留并展示已加载部分（如实标注未扫完）
    pub(super) fn stop_scan(&mut self, cx: &mut Context<Self>) {
        self.scan_generation += 1;
        if self.loading {
            self.loading = false;
            self.truncated = true;
            self.rebuild_tree();
            info!(count = self.keys.len(), "redis scan stopped by user");
        }
        cx.notify();
    }

    /// Enter 下推服务端 MATCH：普通关键字包成 `*kw*`，含 glob 元字符（* ? [）则原样使用；
    /// 空关键字清除模式回到全库扫描
    pub(super) fn apply_server_match(&mut self, cx: &mut Context<Self>) {
        let raw = self.search.read(cx).value().trim().to_string();
        let pattern = if raw.is_empty() {
            None
        } else if raw.contains(['*', '?', '[']) {
            Some(raw)
        } else {
            Some(format!("*{raw}*"))
        };
        // 模式未变且不在扫描中：无需重扫（扫描中按 Enter 视为「以此模式重来」）
        if pattern == self.match_pattern && !self.loading {
            return;
        }
        self.match_pattern = pattern;
        self.refresh(cx);
    }

    /// 扫一批：回包三重校验通过则追加，按需节流重建 Trie 并续扫，扫完 / 达上限收尾
    fn scan_next_batch(
        &self,
        config: ConnectionConfig,
        cursor: u64,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let db = self.db;
        let pattern = self.match_pattern.clone();
        cx.spawn(async move |this, cx| {
            let result = svc
                .scan_batch(&config, db, cursor, pattern.as_deref(), None, SCAN_BATCH)
                .await;
            let _ = this.update(cx, |this, cx| {
                let stale = this.scan_generation != generation
                    || this.db != db
                    || this.config.as_ref().map(|c| &c.id) != Some(&config.id);
                if stale {
                    return;
                }
                match result {
                    Ok(r) => {
                        this.keys.extend(r.keys);
                        let done = r.cursor == 0;
                        let capped = this.keys.len() >= MAX_KEYS;
                        if done || capped {
                            this.loading = false;
                            this.truncated = capped && !done;
                            this.rebuild_tree();
                            info!(count = this.keys.len(), db, capped, "redis scan completed");
                        } else {
                            // 首批立即出树给首屏反馈，此后每积累 REBUILD_STEP 才重建一次
                            if this.last_rebuilt_count == 0
                                || this.keys.len() - this.last_rebuilt_count >= REBUILD_STEP
                            {
                                this.rebuild_tree();
                            }
                            this.scan_next_batch(config, r.cursor, generation, cx);
                        }
                    }
                    Err(e) => {
                        error!(error = %e, db, "redis scan failed");
                        this.loading = false;
                        this.error = Some(format!("加载失败：{e}"));
                        this.keys.clear();
                        this.tree.clear();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
