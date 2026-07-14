//! DB Client 根视图：多连接 Tab，顶部连接 tab bar + 中心 session / picker

mod dialogs;
mod render;

use std::sync::Arc;

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, Point, ScrollHandle, Subscription, Window, px,
};
use ramag_app::{ConnectionService, MongoService, RedisService};
use ramag_domain::entities::{ConnectionConfig, DriverKind};

use ramag_tool_mongodb::MongoSessionPanel;
use ramag_tool_redis::RedisSessionPanel;

use crate::views::connection_list::{ConnectionListPanel, ListEvent};
use crate::views::connection_session::ConnectionSession;

/// 当前主区显示什么
pub(super) enum CenterMode {
    /// 显示某个 Session（active_session 索引）
    Session,
    /// 显示连接管理（保存的连接列表 + 新建）
    ConnectionPicker,
}

/// SQL 类（MySQL / Postgres / 未来 SQLite）走 ConnectionSession；Redis 走 RedisSessionPanel；MongoDB 走 MongoSessionPanel
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
    /// Tab 被激活时触发：各 session 内部按「树为空才补拉」决定是否真正加载，
    /// 既保证「打开就能用」，连接放久后切回也会重新请求（驱动层在取连接时自愈死连接）
    pub(super) fn ensure_loaded(&self, cx: &mut App) {
        match self {
            SessionEntity::Sql(e) => e.update(cx, |s, cx| s.ensure_loaded(cx)),
            SessionEntity::Redis(e) => e.update(cx, |s, cx| s.ensure_loaded(cx)),
            SessionEntity::Mongo(e) => e.update(cx, |s, cx| s.ensure_loaded(cx)),
        }
    }

    /// Tab 激活时聚焦内容，让各会话 cmd-e 等快捷键的 handler 立即在焦点链上（无需先点内容区）
    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            SessionEntity::Sql(e) => e.update(cx, |s, cx| s.focus(window, cx)),
            SessionEntity::Redis(e) => e.update(cx, |s, cx| s.focus(window, cx)),
            SessionEntity::Mongo(e) => e.update(cx, |s, cx| s.focus(window, cx)),
        }
    }
}

/// 顶部一个连接 Tab 对应的槽位。实体惰性创建：跨重启恢复的标签先只有配置，
/// 首次激活才真正建会话连库；连接配置保存后旧实体立即丢弃并置 stale，
/// 由用户在标签内一键重连（草稿按连接 id 持久化，重连自动恢复）
pub(super) struct SessionSlot {
    /// None = 尚未创建（恢复占位 / 配置更新后待重连）
    pub(super) entity: Option<SessionEntity>,
    /// 该槽位当前应使用的连接配置（保存连接后同步为最新）
    pub(super) config: ConnectionConfig,
    /// 配置已更新：旧实体已丢弃，暂停查询与写入，等待用户重连
    pub(super) stale: bool,
}

/// 数据库类型副标签（Tab Bar 副标题；无需实体即可从配置得出）
pub(super) fn driver_kind_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    }
}

pub struct DbClientView {
    pub(super) service: Arc<ConnectionService>,
    pub(super) redis_service: Arc<RedisService>,
    pub(super) mongo_service: Arc<MongoService>,
    /// 已打开的连接会话槽位（含 MySQL + Redis + MongoDB）
    pub(super) sessions: Vec<SessionSlot>,
    /// 当前激活的 session 索引
    pub(super) active_session: Option<usize>,
    /// 中央显示模式
    pub(super) center: CenterMode,
    /// 连接管理面板（始终持有，按需展示）
    pub(super) picker: Entity<ConnectionListPanel>,
    /// 顶部连接 tab bar 横向滚动句柄：连接多到溢出时新开后滚到末尾
    pub(super) sessions_scroll: ScrollHandle,
    /// 异步操作（如删除连接）失败时挂起的提示，render 持 Window 时推送
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    /// 跨重启恢复：启动异步读回上次打开的连接（按保存顺序）与上次激活的连接 id，
    /// render 首帧消费逐个重开（render 才有 Window；不自动连库，树惰性拉取）
    pub(super) pending_restore: Option<(
        Vec<ConnectionConfig>,
        Option<ramag_domain::entities::ConnectionId>,
    )>,
    pub(super) _subscriptions: Vec<Subscription>,
}

/// 打开中的连接列表的偏好 key（JSON：{ids, active}；兼容旧版纯 id 数组）
const OPEN_SESSIONS_PREF: &str = "dbclient_open_sessions";

/// open sessions 偏好的落盘结构；`active` 记录上次激活的连接（重启回到原位）
#[derive(serde::Serialize, serde::Deserialize)]
struct OpenSessionsPref {
    ids: Vec<ramag_domain::entities::ConnectionId>,
    #[serde(default)]
    active: Option<ramag_domain::entities::ConnectionId>,
}

/// 解析偏好：优先新结构，回退旧版纯数组（active 未知）
fn parse_open_sessions(json: &str) -> OpenSessionsPref {
    if let Ok(p) = serde_json::from_str::<OpenSessionsPref>(json) {
        return p;
    }
    OpenSessionsPref {
        ids: serde_json::from_str(json).unwrap_or_default(),
        active: None,
    }
}

impl DbClientView {
    pub fn new(
        service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| {
            ConnectionListPanel::new(
                service.clone(),
                redis_service.clone(),
                mongo_service.clone(),
                window,
                cx,
            )
        });

        let subs = vec![cx.subscribe_in(&picker, window, Self::on_picker_event)];

        // 跨重启恢复：读上次打开的连接 id 列表 + 全部连接配置，按保存顺序匹配，
        // 存入 pending_restore 由 render 首帧（有 Window）逐个重开
        if let Some(storage) = ramag_ui::theme::storage_from_cx(cx) {
            let svc = service.clone();
            cx.spawn(async move |this, cx| {
                let pref = match storage.get_preference(OPEN_SESSIONS_PREF).await {
                    Ok(Some(json)) => parse_open_sessions(&json),
                    _ => return,
                };
                if pref.ids.is_empty() {
                    return;
                }
                let Ok(all) = svc.list().await else {
                    return;
                };
                let configs: Vec<ConnectionConfig> = pref
                    .ids
                    .iter()
                    .filter_map(|id| all.iter().find(|c| &c.id == id).cloned())
                    .collect();
                if configs.is_empty() {
                    return;
                }
                let _ = this.update(cx, |this, cx| {
                    this.pending_restore = Some((configs, pref.active));
                    cx.notify();
                });
            })
            .detach();
        }

        Self {
            service,
            redis_service,
            mongo_service,
            sessions: Vec::new(),
            active_session: None,
            // 启动时显示连接管理（用户挑选打开哪个）
            center: CenterMode::ConnectionPicker,
            picker,
            sessions_scroll: ScrollHandle::new(),
            pending_notification: None,
            pending_restore: None,
            _subscriptions: subs,
        }
    }

    /// 把当前打开的连接 id 列表落 prefs（开 / 关 Session 时调；后台异步，失败仅日志）
    fn persist_open_sessions(&self, cx: &mut Context<Self>) {
        let Some(storage) = ramag_ui::theme::storage_from_cx(cx) else {
            return;
        };
        let ids: Vec<ramag_domain::entities::ConnectionId> =
            self.sessions.iter().map(|s| s.config.id.clone()).collect();
        let active = self
            .active_session
            .and_then(|i| self.sessions.get(i))
            .map(|s| s.config.id.clone());
        let json = match serde_json::to_string(&OpenSessionsPref { ids, active }) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "serialize open sessions failed");
                return;
            }
        };
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = storage.set_preference(OPEN_SESSIONS_PREF, &json).await {
                    tracing::warn!(error = %e, "persist open sessions failed");
                }
            })
            .detach();
    }

    fn on_picker_event(
        &mut self,
        _list: &Entity<ConnectionListPanel>,
        event: &ListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ListEvent::Selected(conn) => {
                // 选中已保存连接 → 打开为新 Session
                self.open_session(conn.clone(), window, cx);
            }
            ListEvent::RequestNew => {
                self.open_form_create(window, cx);
            }
            ListEvent::RequestEdit(conn) => {
                self.open_form_edit(conn.clone(), window, cx);
            }
            ListEvent::RequestDelete(id) => {
                self.confirm_delete(id.clone(), window, cx);
            }
        }
    }

    /// 按 driver 真正创建会话实体（此刻起才会连库 / 拉元数据）
    fn build_session_entity(
        &self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> SessionEntity {
        match config.driver {
            DriverKind::Mysql | DriverKind::Postgres => {
                let svc = self.service.clone();
                let entity = cx.new(|cx| ConnectionSession::new(config, svc, window, cx));
                SessionEntity::Sql(entity)
            }
            DriverKind::Redis => {
                let svc = self.redis_service.clone();
                let entity = cx.new(|cx| RedisSessionPanel::new(config, svc, window, cx));
                SessionEntity::Redis(entity)
            }
            DriverKind::Mongodb => {
                let svc = self.mongo_service.clone();
                let entity = cx.new(|cx| MongoSessionPanel::new(config, svc, window, cx));
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
        let conn_id = config.id.clone();
        let entity = self.build_session_entity(config, window, cx);
        entity.focus(window, cx);
        self.sessions[idx].entity = Some(entity);
        // 真正连库时才异步探测版本（占位标签不建池 / 不试连）
        self.picker
            .update(cx, |p, cx| p.prefetch_version(&conn_id, cx));
        cx.notify();
    }

    /// 配置更新后的一键重连：丢弃 stale 标记，用槽内新配置重建会话
    /// （手写草稿按连接 id 持久化，新会话创建时自动恢复）
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

    /// 打开一个连接作为新 Session（如果已开就切到那个 Tab）
    fn open_session(
        &mut self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            self.persist_open_sessions(cx);
            cx.notify();
            return;
        }

        // 在 config 被 move 进 session 之前先抓 id 用于版本探测
        let conn_id = config.id.clone();
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
        // tab 多溢出时让新连接 tab 滚入视图（GPUI 自动 clamp 到 max_offset）
        self.sessions_scroll
            .set_offset(Point::new(px(-99999.0), px(0.0)));
        // 用户主动打开后才异步探测版本（不打开的连接不会去建池/试连）
        self.picker
            .update(cx, |p, cx| p.prefetch_version(&conn_id, cx));
        self.persist_open_sessions(cx);
        cx.notify();
    }

    /// 首页快捷入口：打开指定已保存连接；若已打开则切换到现有标签。
    pub fn open_connection(
        &mut self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_session(config, window, cx);
    }

    /// 关闭某个 Session Tab
    pub(super) fn close_session(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.sessions.len() {
            return;
        }
        self.sessions.remove(idx);
        // 调整 active
        if self.sessions.is_empty() {
            self.active_session = None;
            self.center = CenterMode::ConnectionPicker;
        } else if let Some(active) = self.active_session {
            if active == idx {
                // 关闭的就是当前激活：切到前一个或 0
                self.active_session = Some(idx.saturating_sub(1).min(self.sessions.len() - 1));
            } else if active > idx {
                // 关闭的在前面：索引减 1
                self.active_session = Some(active - 1);
            }
        }
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
            // active 变化也入偏好：重启后回到上次停留的连接
            self.persist_open_sessions(cx);
            cx.notify();
        }
    }

    /// 切到"打开连接"面板
    pub(super) fn show_picker(&mut self, cx: &mut Context<Self>) {
        self.center = CenterMode::ConnectionPicker;
        // 刷新一下列表
        self.picker.update(cx, |p, cx| p.refresh(cx));
        cx.notify();
    }
}
