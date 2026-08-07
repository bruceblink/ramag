use super::*;

impl ClipboardService {
    /// 读取失败或损坏时暂停采集；仅无已存设置时采用默认值。
    pub async fn load_settings(&self) -> ClipboardSettings {
        // 与保存共用串行锁：防止早先发起的慢读取在新设置保存后才回包，
        // 又把内存快照覆盖回旧值。
        let _guard = self.settings_save_lock.lock().await;
        let settings = match self.storage.get_preference(SETTINGS_KEY).await {
            Ok(Some(json)) => match parse_clipboard_settings(&json) {
                Ok(s) => {
                    self.settings_degraded.store(false, Ordering::Relaxed);
                    s
                }
                Err(e) => {
                    warn!(error = %e, "clipboard settings corrupted; capture disabled");
                    self.settings_degraded.store(true, Ordering::Relaxed);
                    ClipboardSettings {
                        enabled: false,
                        ..ClipboardSettings::default()
                    }
                }
            },
            Ok(None) => {
                self.settings_degraded.store(false, Ordering::Relaxed);
                ClipboardSettings::default()
            }
            Err(e) => {
                warn!(error = %e, "clipboard settings unreadable; capture disabled");
                self.settings_degraded.store(true, Ordering::Relaxed);
                ClipboardSettings {
                    enabled: false,
                    ..ClipboardSettings::default()
                }
            }
        };
        // 与读取同处串行区间内同步运行时镜像，避免启动预热在新保存之后
        // 才把旧值写回内存。
        self.capture_enabled
            .store(settings.enabled, Ordering::Relaxed);
        self.alternate_hotkey
            .store(settings.alternate_hotkey, Ordering::Relaxed);
        self.auto_paste
            .store(settings.auto_paste, Ordering::Relaxed);
        self.cache_settings(&settings);
        settings
    }

    pub fn settings_degraded(&self) -> bool {
        self.settings_degraded.load(Ordering::Relaxed)
    }

    pub async fn save_settings(&self, settings: &ClipboardSettings) -> Result<()> {
        let json = serialize_clipboard_settings(settings)?;
        let _guard = self.settings_save_lock.lock().await;
        // 先更新内存镜像（与 UI 乐观更新一致），热键循环最迟下一拍生效
        let prev_enabled = self
            .capture_enabled
            .swap(settings.enabled, Ordering::Relaxed);
        let prev_alternate = self
            .alternate_hotkey
            .swap(settings.alternate_hotkey, Ordering::Relaxed);
        let prev_auto_paste = self.auto_paste.swap(settings.auto_paste, Ordering::Relaxed);
        let result = self.storage.set_preference(SETTINGS_KEY, &json).await;
        // 持久化失败回滚镜像：内存与落盘不一致会让「界面已关但仍在采集」类偏差跨拍存在
        if result.is_err() {
            self.capture_enabled.store(prev_enabled, Ordering::Relaxed);
            self.alternate_hotkey
                .store(prev_alternate, Ordering::Relaxed);
            self.auto_paste.store(prev_auto_paste, Ordering::Relaxed);
        } else {
            self.cache_settings(settings);
        }
        result
    }

    pub(super) fn cache_settings(&self, settings: &ClipboardSettings) {
        let mut cached = self.settings_cache.write();
        if &*cached != settings {
            *cached = settings.clone();
            self.settings_revision.fetch_add(1, Ordering::Release);
        }
    }

    /// 一致读取快照与版本号。若恰好遇到更新，重试而不返回“旧快照 + 新版本”。
    pub fn settings_snapshot_with_revision(&self) -> (ClipboardSettings, u64) {
        loop {
            let before = self.settings_revision.load(Ordering::Acquire);
            let snapshot = self.settings_cache.read().clone();
            let after = self.settings_revision.load(Ordering::Acquire);
            if before == after {
                return (snapshot, after);
            }
        }
    }

    /// 等待在途保存后读取一致的内存快照。
    pub async fn capture_settings_snapshot(&self) -> ClipboardSettings {
        let _guard = self.settings_save_lock.lock().await;
        self.settings_cache.read().clone()
    }

    pub fn settings_revision(&self) -> u64 {
        self.settings_revision.load(Ordering::Acquire)
    }

    pub fn capture_enabled(&self) -> bool {
        self.capture_enabled.load(Ordering::Relaxed)
    }

    pub fn alternate_hotkey(&self) -> bool {
        self.alternate_hotkey.load(Ordering::Relaxed)
    }

    pub fn auto_paste(&self) -> bool {
        self.auto_paste.load(Ordering::Relaxed)
    }

    pub async fn prime_capture_enabled(&self) -> bool {
        self.load_settings().await.enabled
    }
}
