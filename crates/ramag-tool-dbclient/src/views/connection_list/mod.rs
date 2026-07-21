//! 连接管理页：行点击=打开（emit Selected），行内按钮独立 emit。
//! 搜索按 名称 / 环境标签 / 类型 不区分大小写混合匹配

mod render;
mod row;
mod transfer;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{AppContext as _, Context, Entity, EventEmitter, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use ramag_app::{ConnectionService, MongoService, RedisService};
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DriverKind, contains_case_insensitive,
};
use tracing::{debug, error, info, warn};

pub struct ConnectionListPanel {
    pub(super) service: Arc<ConnectionService>,
    /// Redis 连接的 server_version 走 redis_service
    redis_service: Arc<RedisService>,
    /// MongoDB 连接的 server_version 走 mongo_service
    mongo_service: Arc<MongoService>,
    pub(super) connections: Arc<Vec<ConnectionConfig>>,
    filtered_indices_cache: RefCell<Option<FilteredIndicesCacheEntry>>,
    pub(super) selected: Option<ConnectionId>,
    pub(super) loading: bool,
    /// 加载失败信息：非空时顶部显示错误条，避免把"读取失败"伪装成"没有连接"
    pub(super) load_error: Option<String>,
    pub(super) search: Entity<InputState>,
    /// 小写的搜索关键字
    pub(super) query: String,
    /// 服务端版本缓存。失败连接不入缓存避免重试
    pub(super) versions: HashMap<ConnectionId, String>,
    /// 版本探测状态。底层 Tokio 请求可能在界面任务结束后继续，因此取消只做标记；
    /// 请求结束后会再次清池，确保旧任务不能把资源建回来。
    version_requests: HashMap<ConnectionId, VersionRequest>,
    refresh_generation: u64,
    /// 首次显示时聚焦搜索框（仅一次，不抢用户后续焦点）
    pub(super) focused_search_once: bool,
    /// 导入 / 导出进行中：按钮禁用防重入
    pub(super) transferring: bool,
    /// 待展示的导入导出结果通知（render 时取出弹出）
    pub(super) pending_notification: Option<Notification>,
    _subscriptions: Vec<gpui::Subscription>,
}

struct FilteredIndicesCacheEntry {
    connections: Arc<Vec<ConnectionConfig>>,
    query_lower: String,
    indices: Arc<Vec<usize>>,
}

struct VersionRequest {
    config: ConnectionConfig,
    cancelled: Arc<AtomicBool>,
    /// 配置变化期间若用户已重新连接，旧请求清理完成后再串行探测新配置。
    restart_config: Option<ConnectionConfig>,
}

impl Drop for VersionRequest {
    fn drop(&mut self) {
        // 面板销毁时 detached 请求仍可能在 Tokio 中运行；完成后必须走清理分支。
        self.cancelled.store(true, Ordering::Release);
    }
}

impl FilteredIndicesCacheEntry {
    fn get(
        &self,
        connections: &Arc<Vec<ConnectionConfig>>,
        query_lower: &str,
    ) -> Option<Arc<Vec<usize>>> {
        (Arc::ptr_eq(&self.connections, connections) && self.query_lower == query_lower)
            .then(|| self.indices.clone())
    }
}

#[derive(Debug, Clone)]
pub enum ListEvent {
    Selected(ConnectionConfig),
    RequestNew,
    RequestEdit(ConnectionConfig),
    RequestDelete(ConnectionId),
    /// 原子导入成功后通知根视图清理连接池，并暂停仍持旧配置的打开标签。
    ConnectionsImported(Vec<ConnectionConfig>),
}

impl EventEmitter<ListEvent> for ConnectionListPanel {}

impl ConnectionListPanel {
    pub fn new(
        service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("搜索连接（名称 / 环境 / 类型）")
        });

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = this.search.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            },
        ));

        let mut this = Self {
            service,
            redis_service,
            mongo_service,
            connections: Arc::new(Vec::new()),
            filtered_indices_cache: RefCell::new(None),
            selected: None,
            loading: true,
            load_error: None,
            search,
            query: String::new(),
            versions: HashMap::new(),
            version_requests: HashMap::new(),
            refresh_generation: 0,
            focused_search_once: false,
            transferring: false,
            pending_notification: None,
            _subscriptions: subs,
        };
        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        let generation = self.refresh_generation;
        self.loading = true;
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.list().await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh_generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(list) => {
                        let current_ids: HashSet<ConnectionId> = list
                            .iter()
                            .map(|connection| connection.id.clone())
                            .collect();
                        this.versions.retain(|id, _| current_ids.contains(id));
                        this.connections = Arc::new(list);
                        this.filtered_indices_cache.get_mut().take();
                        this.load_error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "list connections failed");
                        // 保留旧列表（若有）而非清空成假空态，并记错误供顶部提示
                        this.load_error = Some(format!("加载连接列表失败：{e}"));
                    }
                }
                cx.notify();
                // refresh 不批量探测版本，避免对未打开连接反复试连不可达主机；
                // 真正 open_session 时由外层调 prefetch_version
            });
        })
        .detach();
    }

    /// 已缓存则跳过；失败仅 debug
    pub fn prefetch_version(&mut self, conn: &ConnectionConfig, cx: &mut Context<Self>) {
        if self.versions.contains_key(&conn.id) {
            return;
        }
        if let Some(request) = self.version_requests.get_mut(&conn.id) {
            if request.config == *conn {
                // 同配置快速关闭再重开：保留原探测，不重复握手。
                request.cancelled.store(false, Ordering::Release);
                request.restart_config = None;
            } else {
                // 新旧配置不能并发争用同一 ConnectionId 的池；旧请求清理后再启动新请求。
                request.cancelled.store(true, Ordering::Release);
                request.restart_config = Some(conn.clone());
            }
            return;
        }

        let conn = conn.clone();
        let mysql_svc = self.service.clone();
        let redis_svc = self.redis_service.clone();
        let mongo_svc = self.mongo_service.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.version_requests.insert(
            conn.id.clone(),
            VersionRequest {
                config: conn.clone(),
                cancelled: cancelled.clone(),
                restart_config: None,
            },
        );
        cx.spawn(async move |this, cx| {
            let result = match conn.driver {
                DriverKind::Mysql | DriverKind::Postgres => mysql_svc.server_version(&conn).await,
                DriverKind::Redis => redis_svc.server_version(&conn).await,
                DriverKind::Mongodb => mongo_svc.server_version(&conn).await,
            };
            let update_result = this.update(cx, |this, cx| {
                let Some(current) = this.version_requests.get(&conn.id) else {
                    return;
                };
                if !Arc::ptr_eq(&current.cancelled, &cancelled) {
                    return;
                }
                let was_cancelled = cancelled.load(Ordering::Acquire);
                let Some(request) = this.version_requests.remove(&conn.id) else {
                    return;
                };
                let restart = request.restart_config.clone();

                if was_cancelled {
                    evict_version_resources(&mysql_svc, &redis_svc, &mongo_svc, &conn.id);
                } else {
                    match result {
                        Ok(version) => {
                            this.versions.insert(conn.id.clone(), version);
                            cx.notify();
                        }
                        Err(error) => {
                            debug!(error = %error, conn = %conn.name, "fetch server version failed");
                        }
                    }
                }

                if let Some(next) = restart {
                    this.versions.remove(&next.id);
                    this.prefetch_version(&next, cx);
                }
            });
            if update_result.is_err() {
                // 面板已销毁：没有任何会话需要保留此次探测建立的资源。
                evict_version_resources(&mysql_svc, &redis_svc, &mongo_svc, &conn.id);
            }
        })
        .detach();
    }

    /// 关闭连接标签时标记探测不再需要；底层请求完成后会再次清池。
    pub fn cancel_version_prefetch(&mut self, id: &ConnectionId) {
        if let Some(request) = self.version_requests.get_mut(id) {
            request.cancelled.store(true, Ordering::Release);
            request.restart_config = None;
        }
    }

    /// 配置变化时版本文案也失效，并让旧探测进入“完成后清池”分支。
    pub fn invalidate_version(&mut self, id: &ConnectionId) {
        self.versions.remove(id);
        self.cancel_version_prefetch(id);
    }

    pub fn connections(&self) -> &[ConnectionConfig] {
        self.connections.as_slice()
    }

    pub(super) fn handle_click(&mut self, conn: ConnectionConfig, cx: &mut Context<Self>) {
        self.selected = Some(conn.id.clone());
        cx.emit(ListEvent::Selected(conn));
        cx.notify();
    }

    /// 导出：单框自定义口令（≥8 字符，校验失败内联提示），
    /// 明文显隐切换供自查，免二次输入确认
    pub(super) fn prompt_export_passphrase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transferring {
            return;
        }
        let entity = cx.entity().clone();
        ramag_ui::open_reveal_masked_prompt(
            "导出连接配置",
            "设置至少 8 个字符的自定义口令。文件包含数据库密码，遗失口令将无法恢复。",
            "导出",
            |passphrase| transfer::validate_export_passphrase(passphrase).err(),
            move |passphrase, _, app| {
                entity.update(app, |this, cx| this.export_connections(passphrase, cx));
            },
            window,
            cx,
        );
    }

    /// 加密导出全部连接（PBKDF2 口令派生 + AES-256-GCM）；文件名带时间戳避免互相覆盖
    fn export_connections(&mut self, passphrase: String, cx: &mut Context<Self>) {
        if self.transferring {
            return;
        }
        self.transferring = true;
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome: Result<Option<String>, String> = async {
                let connections = svc
                    .list()
                    .await
                    .map_err(|error| format!("读取连接列表失败：{error}"))?;
                let file_name = format!(
                    "ramag-connections-{}.json",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                );
                let Some(handle) = rfd::AsyncFileDialog::new()
                    .set_file_name(&file_name)
                    .add_filter("Ramag JSON", &["json"])
                    .save_file()
                    .await
                else {
                    return Ok(None);
                };
                let path = handle.path().to_path_buf();
                let write_path = path.clone();
                // 密钥派生与加密为 CPU 密集，连同写盘一起放阻塞线程
                ramag_app::run_blocking(move || {
                    let content = transfer::encrypt_connections(&connections, &passphrase)
                        .map_err(ramag_domain::error::DomainError::Other)?;
                    ramag_app::usecases::export::write_atomic(&write_path, &content)
                })
                .await
                .map_err(|error| format!("写入导出文件失败：{error}"))?;
                Ok(Some(path.display().to_string()))
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.transferring = false;
                match outcome {
                    // 用户取消保存框：静默返回
                    Ok(None) => {}
                    Ok(Some(path)) => {
                        info!(path = %path, "connections exported");
                        this.pending_notification =
                            Some(Notification::success(format!("已加密导出到 {path}")));
                    }
                    Err(error) => {
                        error!(error = %error, "export connections failed");
                        this.pending_notification = Some(Notification::error(error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 导入：文件读取、格式识别和解析均在阻塞线程；覆盖前确认，保存时单事务提交。
    pub(super) fn import_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transferring {
            return;
        }
        self.transferring = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let picked: Result<Option<transfer::PreparedImport>, String> = async {
                let Some(handle) = rfd::AsyncFileDialog::new()
                    .add_filter("Ramag JSON（兼容旧 .yaml）", &["json", "yaml", "yml"])
                    .pick_file()
                    .await
                else {
                    return Ok(None);
                };
                let read_path = handle.path().to_path_buf();
                let prepared = ramag_app::run_blocking(move || {
                    let file = std::fs::File::open(&read_path).map_err(|error| {
                        ramag_domain::error::DomainError::Storage(format!(
                            "打开导入文件失败：{error}"
                        ))
                    })?;
                    let meta = file.metadata().map_err(|error| {
                        ramag_domain::error::DomainError::Storage(format!(
                            "读取文件信息失败：{error}"
                        ))
                    })?;
                    if !meta.is_file() {
                        return Err(ramag_domain::error::DomainError::InvalidConfig(
                            "导入目标必须是普通文件".into(),
                        ));
                    }
                    if meta.len() > transfer::MAX_IMPORT_FILE_BYTES {
                        return Err(ramag_domain::error::DomainError::Storage(format!(
                            "文件过大：{} bytes，最多 {} bytes",
                            meta.len(),
                            transfer::MAX_IMPORT_FILE_BYTES
                        )));
                    }
                    let mut raw = String::new();
                    let mut limited = file.take(transfer::MAX_IMPORT_FILE_BYTES + 1);
                    limited.read_to_string(&mut raw).map_err(|error| {
                        ramag_domain::error::DomainError::Storage(format!(
                            "读取导入文件失败：{error}"
                        ))
                    })?;
                    if raw.len() as u64 > transfer::MAX_IMPORT_FILE_BYTES {
                        return Err(ramag_domain::error::DomainError::Storage(format!(
                            "文件读取过程中超过 {} bytes 上限",
                            transfer::MAX_IMPORT_FILE_BYTES
                        )));
                    }
                    transfer::prepare_import(raw).map_err(ramag_domain::error::DomainError::Other)
                })
                .await
                .map_err(|error| format!("读取导入文件失败：{error}"))?;
                Ok(Some(prepared))
            }
            .await;

            match picked {
                Ok(None) => {
                    let _ = this.update(cx, |this, cx| {
                        this.transferring = false;
                        cx.notify();
                    });
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.transferring = false;
                        error!(error = %error, "import connections failed");
                        this.pending_notification = Some(Notification::error(error));
                        cx.notify();
                    });
                }
                Ok(Some(transfer::PreparedImport::Plain { valid, skipped })) => {
                    // transferring 保持 true，待覆盖数量算出后在合并确认框前复位
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.confirm_plain_import(valid, skipped, window, cx);
                    });
                }
                Ok(Some(transfer::PreparedImport::Encrypted(raw))) => {
                    // 口令框可被取消且无取消回调，打开前先复位，避免状态永久卡住。
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.transferring = false;
                        cx.notify();
                        let entity = cx.entity().clone();
                        ramag_ui::open_bounded_masked_prompt(
                            "输入导入口令",
                            "请输入导出时设置的口令。旧版默认口令文件仍可手动输入原口令解密。",
                            "",
                            "解密导入",
                            transfer::MAX_TRANSFER_PASSPHRASE_BYTES,
                            move |passphrase, window, app| {
                                entity.update(app, |this, cx| {
                                    this.decrypt_and_import(raw, passphrase, window, cx)
                                });
                            },
                            window,
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
    }

    /// 明文 V1 导入：一次确认同时给出明文风险与新增 / 覆盖数量，
    /// 替代「明文警告 → 覆盖确认」两个背靠背弹框；进入时 transferring 仍为 true
    fn confirm_plain_import(
        &mut self,
        valid: Vec<ConnectionConfig>,
        skipped: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let existing = svc.list().await;
            let _ = this.update_in(cx, move |this, window, cx| {
                this.transferring = false;
                cx.notify();
                match existing {
                    Ok(existing) => {
                        let (added, overwritten) = import_change_counts(&valid, &existing);
                        let mut description = format!(
                            "这是旧版 V1 明文文件，可能直接包含数据库密码。请仅导入可信文件，\
                             并在完成后安全删除原文件。\n\n将新增 {added} 个连接"
                        );
                        if overwritten > 0 {
                            description.push_str(&format!(
                                "、覆盖 {overwritten} 个同 ID 连接\
                                 （相关连接池会清理，已打开标签需重新连接）"
                            ));
                        }
                        if !skipped.is_empty() {
                            description
                                .push_str(&format!("，另跳过 {} 个无效或重复条目", skipped.len()));
                        }
                        description.push('。');
                        let entity = cx.entity().clone();
                        ramag_ui::open_confirm(
                            "导入未加密配置？",
                            description,
                            "继续导入",
                            true,
                            move |_, app| {
                                entity
                                    .update(app, |this, cx| this.save_imported(valid, skipped, cx));
                            },
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        error!(error = %error, "load connections before import failed");
                        this.pending_notification = Some(Notification::error(format!(
                            "导入前读取现有连接失败：{error}"
                        )));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 解密加密导出文件并导入（KDF 为 CPU 密集，放阻塞线程）
    fn decrypt_and_import(
        &mut self,
        raw: String,
        passphrase: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transferring {
            return;
        }
        self.transferring = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = ramag_app::run_blocking(move || {
                transfer::decrypt_connections(&raw, &passphrase)
                    .map_err(ramag_domain::error::DomainError::Other)
            })
            .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((valid, skipped)) => this.prepare_import_save(valid, skipped, window, cx),
                Err(error) => {
                    this.transferring = false;
                    error!(error = %error, "decrypt import file failed");
                    this.pending_notification =
                        Some(Notification::error(format!("解密失败：{error}")));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 读取当前连接并计算新增 / 覆盖数量；存在覆盖时必须由用户确认。
    fn prepare_import_save(
        &mut self,
        valid: Vec<ConnectionConfig>,
        skipped: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transferring = true;
        cx.notify();
        let svc = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let existing = svc.list().await;
            let _ = this.update_in(cx, move |this, window, cx| match existing {
                Ok(existing) => {
                    let (added, overwritten) = import_change_counts(&valid, &existing);
                    if overwritten == 0 {
                        this.save_imported(valid, skipped, cx);
                        return;
                    }

                    this.transferring = false;
                    cx.notify();
                    let skipped_count = skipped.len();
                    let entity = cx.entity().clone();
                    ramag_ui::open_confirm(
                        "覆盖现有连接？",
                        format!(
                            "将新增 {added} 个连接、覆盖 {overwritten} 个同 ID 连接{}。覆盖成功后，相关连接池会清理，已打开标签需重新连接。",
                            if skipped_count == 0 {
                                String::new()
                            } else {
                                format!("，另跳过 {skipped_count} 个无效或重复条目")
                            }
                        ),
                        "继续导入",
                        true,
                        move |_, app| {
                            entity.update(app, |this, cx| {
                                this.save_imported(valid, skipped, cx)
                            });
                        },
                        window,
                        cx,
                    );
                }
                Err(error) => {
                    this.transferring = false;
                    error!(error = %error, "load connections before import failed");
                    this.pending_notification = Some(Notification::error(format!(
                        "导入前读取现有连接失败：{error}"
                    )));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 原子保存导入连接；成功后通知根视图清理缓存并暂停仍持旧配置的标签。
    fn save_imported(
        &mut self,
        valid: Vec<ConnectionConfig>,
        skipped: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.transferring = true;
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.save_many(&valid).await;
            let _ = this.update(cx, move |this, cx| {
                this.transferring = false;
                for entry in &skipped {
                    warn!(entry = %entry, "import entry skipped");
                }
                match result {
                    Ok(()) => {
                        let imported = valid.len();
                        info!(
                            imported,
                            skipped = skipped.len(),
                            "connections imported atomically"
                        );
                        for config in &valid {
                            this.invalidate_version(&config.id);
                        }
                        let message = if imported == 0 {
                            if skipped.is_empty() {
                                "导入文件中没有连接".to_string()
                            } else {
                                format!("没有可导入的连接，已跳过 {} 个条目", skipped.len())
                            }
                        } else if skipped.is_empty() {
                            format!("已导入 {imported} 个连接")
                        } else {
                            format!(
                                "已导入 {imported} 个连接，跳过 {} 个（原因见日志）",
                                skipped.len()
                            )
                        };
                        this.pending_notification = Some(if imported > 0 && skipped.is_empty() {
                            Notification::success(message)
                        } else {
                            Notification::warning(message)
                        });
                        if !valid.is_empty() {
                            cx.emit(ListEvent::ConnectionsImported(valid));
                            this.refresh(cx);
                        }
                    }
                    Err(error) => {
                        error!(error = %error, "atomic import connections failed");
                        this.pending_notification = Some(Notification::error(format!(
                            "导入失败，未写入任何连接：{error}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn filtered_indices(&self) -> Arc<Vec<usize>> {
        {
            let cache = self.filtered_indices_cache.borrow();
            if let Some(indices) = cache
                .as_ref()
                .and_then(|entry| entry.get(&self.connections, &self.query))
            {
                return indices;
            }
        }

        let q = &self.query;
        let indices: Arc<Vec<usize>> = Arc::new(
            self.connections
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    contains_case_insensitive(&c.name, q)
                        || c.environment
                            .as_deref()
                            .is_some_and(|environment| contains_case_insensitive(environment, q))
                        || contains_case_insensitive(driver_search_label(c.driver), q)
                })
                .map(|(index, _)| index)
                .collect(),
        );
        self.filtered_indices_cache
            .replace(Some(FilteredIndicesCacheEntry {
                connections: self.connections.clone(),
                query_lower: self.query.clone(),
                indices: indices.clone(),
            }));
        indices
    }
}

fn import_change_counts(
    incoming: &[ConnectionConfig],
    existing: &[ConnectionConfig],
) -> (usize, usize) {
    let existing_ids: HashSet<ConnectionId> =
        existing.iter().map(|config| config.id.clone()).collect();
    incoming
        .iter()
        .fold((0, 0), |(added, overwritten), config| {
            if existing_ids.contains(&config.id) {
                (added, overwritten + 1)
            } else {
                (added + 1, overwritten)
            }
        })
}

/// 搜索用类型名（与列表类型徽章文案一致，支持按 "mysql" / "redis" 等过滤）
fn driver_search_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    }
}

fn evict_version_resources(
    sql: &ConnectionService,
    redis: &RedisService,
    mongo: &MongoService,
    id: &ConnectionId,
) {
    sql.evict_all_pools(id);
    redis.evict_pool(id);
    mongo.evict_pool(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_indices_cache_requires_same_source_and_query() {
        let connections = Arc::new(Vec::new());
        let indices = Arc::new(Vec::new());
        let cache = FilteredIndicesCacheEntry {
            connections: connections.clone(),
            query_lower: "local".into(),
            indices: indices.clone(),
        };

        let cached = cache.get(&connections, "local");
        assert!(
            cached
                .as_ref()
                .is_some_and(|value| Arc::ptr_eq(value, &indices))
        );
        assert!(cache.get(&connections, "remote").is_none());
        assert!(cache.get(&Arc::new(Vec::new()), "local").is_none());
    }

    #[test]
    fn dropping_version_request_marks_background_cleanup_required() {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let _request = VersionRequest {
                config: ConnectionConfig::new_mysql("local", "127.0.0.1", 3306, "root"),
                cancelled: cancelled.clone(),
                restart_config: None,
            };
        }

        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn import_change_counts_separates_new_and_overwritten_ids() {
        let existing = vec![ConnectionConfig::new_mysql(
            "existing",
            "127.0.0.1",
            3306,
            "root",
        )];
        let mut overwritten = existing[0].clone();
        overwritten.name = "updated".into();
        let added = ConnectionConfig::new_redis("new", "127.0.0.1", 6379);

        assert_eq!(
            import_change_counts(&[overwritten, added], &existing),
            (1, 1)
        );
    }
}
