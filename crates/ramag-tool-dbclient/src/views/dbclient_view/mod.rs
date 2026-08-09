mod dialogs;
mod lifecycle;
mod render;

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, Point, ScrollHandle, Subscription, Window, px,
};
use ramag_app::{ConnectionService, DataSyncService, MongoService, RedisService};
use ramag_domain::entities::{ConnectionConfig, ConnectionId, DriverKind};

use ramag_tool_mongodb::MongoSessionPanel;
use ramag_tool_redis::RedisSessionPanel;

use crate::views::connection_list::{ConnectionListPanel, ListEvent};
use crate::views::connection_session::ConnectionSession;

pub(super) enum CenterMode {
    Session,
    ConnectionPicker,
}

/// 各数据库会话的统一视图句柄。
pub(super) enum SessionEntity {
    Sql(Entity<ConnectionSession>),
    Redis(Entity<RedisSessionPanel>),
    Mongo(Entity<MongoSessionPanel>),
}

impl SessionEntity {
    /// 元数据树状态 (loading, has_error)：不能等同于实时连接健康。
    pub(super) fn health(&self, cx: &App) -> (bool, bool) {
        match self {
            SessionEntity::Sql(e) => e.read(cx).health(cx),
            SessionEntity::Redis(e) => e.read(cx).health(cx),
            SessionEntity::Mongo(e) => e.read(cx).health(cx),
        }
    }
    pub(super) fn to_any_view(&self) -> AnyView {
        match self {
            SessionEntity::Sql(e) => e.clone().into(),
            SessionEntity::Redis(e) => e.clone().into(),
            SessionEntity::Mongo(e) => e.clone().into(),
        }
    }
    /// 仅在元数据尚未成功加载时补拉。
    pub(super) fn ensure_loaded(&self, cx: &mut App) {
        match self {
            SessionEntity::Sql(e) => e.update(cx, |s, cx| s.ensure_loaded(cx)),
            SessionEntity::Redis(e) => e.update(cx, |s, cx| s.ensure_loaded(cx)),
            SessionEntity::Mongo(e) => e.update(cx, |s, cx| s.ensure_loaded(cx)),
        }
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            SessionEntity::Sql(e) => e.update(cx, |s, cx| s.focus(window, cx)),
            SessionEntity::Redis(e) => e.update(cx, |s, cx| s.focus(window, cx)),
            SessionEntity::Mongo(e) => e.update(cx, |s, cx| s.focus(window, cx)),
        }
    }

    pub(super) fn set_result_active(&self, active: bool, cx: &mut App) {
        match self {
            SessionEntity::Sql(entity) => {
                entity.update(cx, |session, cx| session.set_result_active(active, cx))
            }
            SessionEntity::Mongo(entity) => {
                entity.update(cx, |session, cx| session.set_result_active(active, cx))
            }
            SessionEntity::Redis(_) => {}
        }
    }
}

/// 会话实体首次激活才创建；配置变化后须由用户重连。
pub(super) struct SessionSlot {
    pub(super) entity: Option<SessionEntity>,
    pub(super) config: ConnectionConfig,
    pub(super) stale: bool,
}

pub(super) fn driver_kind_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    }
}

pub(super) fn evict_connection_resources(
    service: &ConnectionService,
    redis_service: &RedisService,
    mongo_service: &MongoService,
    id: &ConnectionId,
) {
    // 连接配置允许切换 driver；关闭或更新时必须清理旧、新所有类型的缓存。
    service.evict_all_pools(id);
    redis_service.evict_pool(id);
    mongo_service.evict_pool(id);
}

pub struct DbClientView {
    pub(super) service: Arc<ConnectionService>,
    pub(super) redis_service: Arc<RedisService>,
    pub(super) mongo_service: Arc<MongoService>,
    pub(super) data_sync_service: Arc<DataSyncService>,
    pub(super) result_memory: ramag_ui::ResultMemoryBudget,
    pub(super) sessions: Vec<SessionSlot>,
    pub(super) active_session: Option<usize>,
    pub(super) center: CenterMode,
    pub(super) picker: Entity<ConnectionListPanel>,
    pub(super) sessions_scroll: ScrollHandle,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    /// 跨重启恢复：启动异步读回上次打开的连接（按保存顺序）与上次激活的连接 id，
    /// render 首帧消费逐个重开（render 才有 Window；不自动连库，树惰性拉取）
    pub(super) pending_restore: Option<(
        Vec<ConnectionConfig>,
        Option<ramag_domain::entities::ConnectionId>,
    )>,
    /// 启动恢复只在用户尚未手动改动标签时生效；慢回包不得覆盖用户刚做出的选择。
    pub(super) restore_allowed: bool,
    pub(super) form_subscription: Option<Subscription>,
    pub(super) _subscriptions: Vec<Subscription>,
}

const OPEN_SESSIONS_PREF: &str = "dbclient_open_sessions";
/// 单窗口连接会话上限；每个实体都可能持有连接池、元数据树、编辑器与后台订阅。
const MAX_CONNECTION_SESSIONS: usize = 32;
/// 恢复偏好只应包含少量 UUID；先限制原始 JSON，避免异常数据放大反序列化成本。
const MAX_OPEN_SESSIONS_PREF_BYTES: usize = 64 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct OpenSessionsPref {
    ids: Vec<ramag_domain::entities::ConnectionId>,
    #[serde(default)]
    active: Option<ramag_domain::entities::ConnectionId>,
}

/// 解析并归一化偏好：兼容旧版数组，去重并只保留前 N 个有效槽位。
/// 返回值第二项表示输入包含重复、超限或失效 active，调用方应给用户提示。
fn parse_open_sessions(json: &str) -> Result<(OpenSessionsPref, bool), String> {
    if json.len() > MAX_OPEN_SESSIONS_PREF_BYTES {
        return Err(format!("连接标签恢复数据过大：{} bytes", json.len()));
    }
    let mut pref = if let Ok(pref) = serde_json::from_str::<OpenSessionsPref>(json) {
        pref
    } else {
        let ids = serde_json::from_str(json)
            .map_err(|error| format!("解析连接标签恢复数据失败：{error}"))?;
        OpenSessionsPref { ids, active: None }
    };

    let original_len = pref.ids.len();
    let mut seen = HashSet::with_capacity(original_len.min(MAX_CONNECTION_SESSIONS));
    let mut ids = Vec::with_capacity(original_len.min(MAX_CONNECTION_SESSIONS));
    for id in std::mem::take(&mut pref.ids) {
        if seen.insert(id.clone()) && ids.len() < MAX_CONNECTION_SESSIONS {
            ids.push(id);
        }
    }
    let mut adjusted = ids.len() != original_len;
    pref.ids = ids;
    if pref
        .active
        .as_ref()
        .is_some_and(|active| !pref.ids.contains(active))
    {
        pref.active = None;
        adjusted = true;
    }
    Ok((pref, adjusted))
}

impl DbClientView {
    /// 把当前打开的连接 id 列表落 prefs；同 key 串行且只保留最新快照，避免快速切换写乱序。
    fn persist_open_sessions(&self, cx: &mut Context<Self>) {
        let ids: Vec<ramag_domain::entities::ConnectionId> =
            self.sessions.iter().map(|s| s.config.id.clone()).collect();
        let active = self
            .active_session
            .and_then(|i| self.sessions.get(i))
            .map(|s| s.config.id.clone());
        let json = match serde_json::to_string(&OpenSessionsPref { ids, active }) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(
                    operation = "dbclient_session_restore",
                    stage = "serialize",
                    error = %e,
                    "serialize open sessions failed"
                );
                return;
            }
        };
        ramag_ui::preferences::persist_preference_latest(OPEN_SESSIONS_PREF, json, cx);
    }

    fn on_picker_event(
        &mut self,
        _list: &Entity<ConnectionListPanel>,
        event: &ListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.data_sync_service.gate().is_blocking()
            && !matches!(event, ListEvent::ConnectionsChanged(_))
        {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(
                    "数据同步占用应用中，请等待完成并确认结果",
                )
                .autohide(true),
            );
            cx.notify();
            return;
        }
        match event {
            ListEvent::Selected(conn) => {
                // 选中已保存连接 → 打开为新 Session
                self.open_session(conn.clone(), window, cx);
            }
            ListEvent::RequestNew => {
                self.open_form_create(window, cx);
            }
            ListEvent::RequestSync(target) => {
                let connections = _list.read(cx).connections.clone();
                self.open_data_sync(target.clone(), &connections, window, cx);
            }
            ListEvent::RequestEdit(conn) => {
                self.open_form_edit(conn.clone(), window, cx);
            }
            ListEvent::RequestDelete(id) => {
                self.confirm_delete(id.clone(), window, cx);
            }
            ListEvent::ConnectionsChanged(configs) => {
                let mut any_stale = false;
                for config in configs {
                    evict_connection_resources(
                        &self.service,
                        &self.redis_service,
                        &self.mongo_service,
                        &config.id,
                    );
                    any_stale |= self.mark_sessions_stale(config);
                }
                if any_stale {
                    self.pending_notification = Some(
                        gpui_component::notification::Notification::info(
                            "连接配置已更新，相关标签已暂停，请重新连接后继续操作",
                        )
                        .autohide(true),
                    );
                }
                cx.notify();
            }
        }
    }

    fn build_session_entity(
        &self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> SessionEntity {
        match config.driver {
            DriverKind::Mysql | DriverKind::Postgres => {
                let svc = self.service.clone();
                let budget = self.result_memory.clone();
                let entity = cx.new(|cx| ConnectionSession::new(config, svc, budget, window, cx));
                SessionEntity::Sql(entity)
            }
            DriverKind::Redis => {
                let svc = self.redis_service.clone();
                let entity = cx.new(|cx| RedisSessionPanel::new(config, svc, window, cx));
                SessionEntity::Redis(entity)
            }
            DriverKind::Mongodb => {
                let svc = self.mongo_service.clone();
                let budget = self.result_memory.clone();
                let entity = cx.new(|cx| MongoSessionPanel::new(config, svc, budget, window, cx));
                SessionEntity::Mongo(entity)
            }
        }
    }

    /// 惰性占位槽首次激活：按槽内配置建实体并聚焦（stale 槽不在此建，由重连按钮触发）
    pub(super) fn materialize_slot(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.sessions.get(idx) else {
            return;
        };
        if slot.entity.is_some() || slot.stale {
            return;
        }
        let config = slot.config.clone();
        let entity = self.build_session_entity(config.clone(), window, cx);
        entity.focus(window, cx);
        self.sessions[idx].entity = Some(entity);
        self.sync_result_activity(cx);
        // 真正连库时才异步探测版本（占位标签不建池 / 不试连）
        self.picker
            .update(cx, |p, cx| p.prefetch_version(&config, cx));
        cx.notify();
    }

    pub(super) fn reconnect_slot(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.sessions.get_mut(idx) else {
            return;
        };
        slot.stale = false;
        slot.entity = None;
        self.materialize_slot(idx, window, cx);
    }

    fn open_session(
        &mut self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restore_allowed = false;
        self.pending_restore = None;
        // 已开过的话直接切过去；stale 槽视为用户明确要求按新配置连接，直接重连
        if let Some(idx) = self.sessions.iter().position(|s| s.config.id == config.id) {
            self.active_session = Some(idx);
            self.center = CenterMode::Session;
            if self.sessions[idx].stale {
                self.reconnect_slot(idx, window, cx);
            } else if self.sessions[idx].entity.is_none() {
                self.materialize_slot(idx, window, cx);
            } else if let Some(entity) = &self.sessions[idx].entity {
                // 重新激活已打开的连接：树为空则补拉（含首次加载失败后的重试）
                entity.ensure_loaded(cx);
                // 聚焦内容，cmd-e 等快捷键无需先点内容区
                entity.focus(window, cx);
            }
            self.sync_result_activity(cx);
            self.persist_open_sessions(cx);
            cx.notify();
            return;
        }

        if self.sessions.len() >= MAX_CONNECTION_SESSIONS {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "连接标签已达上限（{MAX_CONNECTION_SESSIONS} 个），请先关闭不需要的标签"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }

        // 在 config 被 move 进 session 之前保留版本探测快照。
        let version_config = config.clone();
        let entity = self.build_session_entity(config.clone(), window, cx);
        // 新会话立即聚焦内容，cmd-e 等快捷键无需先点内容区
        entity.focus(window, cx);
        self.sessions.push(SessionSlot {
            entity: Some(entity),
            config,
            stale: false,
        });
        self.active_session = Some(self.sessions.len() - 1);
        self.center = CenterMode::Session;
        self.sync_result_activity(cx);
        // tab 多溢出时让新连接 tab 滚入视图（GPUI 自动 clamp 到 max_offset）
        self.sessions_scroll
            .set_offset(Point::new(px(-99999.0), px(0.0)));
        // 用户主动打开后才异步探测版本（不打开的连接不会去建池/试连）
        self.picker
            .update(cx, |p, cx| p.prefetch_version(&version_config, cx));
        self.persist_open_sessions(cx);
        cx.notify();
    }

    pub(super) fn close_session(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.sessions.len() {
            return;
        }
        self.restore_allowed = false;
        self.pending_restore = None;
        let SessionSlot {
            entity,
            config,
            stale: _,
        } = self.sessions.remove(idx);
        // 先释放视图及其后台 ticker，再清连接池 / SSH 隧道。
        drop(entity);
        self.picker
            .update(cx, |picker, _| picker.cancel_version_prefetch(&config.id));
        evict_connection_resources(
            &self.service,
            &self.redis_service,
            &self.mongo_service,
            &config.id,
        );
        if self.sessions.is_empty() {
            self.active_session = None;
            self.center = CenterMode::ConnectionPicker;
        } else if let Some(active) = self.active_session {
            if active == idx {
                self.active_session = Some(idx.saturating_sub(1).min(self.sessions.len() - 1));
            } else if active > idx {
                self.active_session = Some(active - 1);
            }
        }
        self.sync_result_activity(cx);
        self.persist_open_sessions(cx);
        cx.notify();
    }

    pub(super) fn select_session(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if idx < self.sessions.len() {
            self.active_session = Some(idx);
            self.center = CenterMode::Session;
            if self.sessions[idx].stale {
                // 配置已更新：不建实体不聚焦，中央区显示"重新连接"面板
            } else if self.sessions[idx].entity.is_none() {
                // 惰性占位首次激活：此刻才建会话连库
                self.materialize_slot(idx, window, cx);
            } else if let Some(entity) = &self.sessions[idx].entity {
                // 切到该 Tab：树为空则补拉（含首次加载失败后的重试）
                entity.ensure_loaded(cx);
                // 聚焦内容，cmd-e 等快捷键无需先点内容区
                entity.focus(window, cx);
            }
            self.sync_result_activity(cx);
            // active 变化也入偏好：重启后回到上次停留的连接
            self.persist_open_sessions(cx);
            cx.notify();
        }
    }

    pub(super) fn show_picker(&mut self, cx: &mut Context<Self>) {
        self.center = CenterMode::ConnectionPicker;
        self.sync_result_activity(cx);
        self.picker.update(cx, |p, cx| p.refresh(cx));
        cx.notify();
    }

    pub(super) fn sync_result_activity(&self, cx: &mut Context<Self>) {
        let showing_session = matches!(self.center, CenterMode::Session);
        for (index, slot) in self.sessions.iter().enumerate() {
            if let Some(entity) = &slot.entity {
                entity.set_result_active(showing_session && self.active_session == Some(index), cx);
            }
        }
    }
}

impl Drop for DbClientView {
    fn drop(&mut self) {
        let service = self.service.clone();
        let redis_service = self.redis_service.clone();
        let mongo_service = self.mongo_service.clone();
        for SessionSlot {
            entity,
            config,
            stale: _,
        } in self.sessions.drain(..)
        {
            drop(entity);
            evict_connection_resources(&service, &redis_service, &mongo_service, &config.id);
        }
    }
}

#[cfg(test)]
mod preference_tests;
