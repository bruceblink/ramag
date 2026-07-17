//! 连接新增 / 编辑表单。在 DbClientView 内嵌入，提供「测试连接」+「保存」

use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement, Styled,
    Subscription, Window, div, px,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use ramag_app::{ConnectionService, MongoService, RedisService};
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DriverKind, MAX_CONNECTION_HOST_BYTES,
    MAX_CONNECTION_IDENTIFIER_BYTES, MAX_CONNECTION_NAME_BYTES, MAX_CONNECTION_PASSWORD_BYTES,
    MAX_CONNECTION_PATH_BYTES, MAX_CONNECTION_SSH_TARGET_BYTES,
};

/// 表单模式：新增 or 编辑
#[derive(Debug, Clone)]
pub enum FormMode {
    Create,
    Edit(ConnectionId),
}

/// 测试结果
#[derive(Debug, Clone)]
pub(super) enum TestState {
    Idle,
    Testing,
    Success,
    Failed(String),
}

/// 表单事件
#[derive(Debug, Clone)]
pub enum FormEvent {
    /// 用户保存成功（Box 压缩枚举尺寸，config 已有十余字段）
    Saved(Box<ConnectionConfig>),
    /// 用户取消
    Cancelled,
}

/// driver 元数据 (id, 显示名, 当前可用)。UI 选择器按此顺序从左到右渲染
const DRIVERS: &[(&str, &str, bool)] = &[
    ("mysql", "MySQL", true),
    ("postgres", "PostgreSQL", true),
    ("redis", "Redis", true),
    ("mongodb", "MongoDB", true),
];

const MAX_PORT_TEXT_BYTES: usize = 5;

fn bounded_input(
    max_bytes: usize,
    window: &mut Window,
    cx: &mut Context<InputState>,
) -> InputState {
    InputState::new(window, cx).validate(move |value, _| value.len() <= max_bytes)
}

/// 连接表单面板
pub struct ConnectionFormPanel {
    service: Arc<ConnectionService>,
    /// Redis 服务（test_connection 时按 driver 路由）；Storage 与 service 共用
    redis_service: Arc<RedisService>,
    /// MongoDB 服务（同上，按 driver 路由）
    mongo_service: Arc<MongoService>,
    pub(super) mode: FormMode,
    /// 当前选中的 driver id（"mysql" / "postgres" / ...）
    pub(super) driver_id: &'static str,
    pub(super) name: Entity<InputState>,
    pub(super) host: Entity<InputState>,
    pub(super) port: Entity<InputState>,
    pub(super) username: Entity<InputState>,
    pub(super) password: Entity<InputState>,
    /// 密码显示状态仅影响界面，不属于连接配置或脏检测内容。
    pub(super) password_masked: bool,
    pub(super) database: Entity<InputState>,
    /// MongoDB 认证库（authSource）输入框；仅 MongoDB 渲染，留空 = admin
    pub(super) auth_source: Entity<InputState>,
    /// 当前表单不渲染备注字段；编辑时仍需原样保留，避免静默丢失历史数据。
    pub(super) remark: Option<String>,
    /// 生产模式：开启后该连接由 driver 层拦截一切写 / 改 / 删操作（只读保护）
    pub(super) production: bool,
    /// 启用 TLS 加密传输
    pub(super) tls: bool,
    /// TLS 身份验证等级（Full 推荐 / Ca / None 仅加密）
    pub(super) tls_verify: ramag_domain::entities::TlsVerify,
    /// 自定义 CA 证书路径输入框（PEM，可选；仅 tls 开启时渲染）
    pub(super) ca_cert_path: Entity<InputState>,
    /// MongoDB URI 粘贴框（仅 MongoDB 渲染）：解析回填 host/port/凭证/库/authSource/tls
    pub(super) mongo_uri: Entity<InputState>,
    /// SSH 跳板目标（user@host / 别名；留空不走隧道，认证由系统 ssh 处理）
    pub(super) ssh_target: Entity<InputState>,
    /// SSH 跳板端口（留空 = 22 或 ~/.ssh/config 决定）
    pub(super) ssh_port: Entity<InputState>,
    pub(super) test_state: TestState,
    /// 测试结果代次：连接参数变更即递增，在途测试结果代次不符则丢弃
    pub(super) test_epoch: u64,
    pub(super) saving: bool,
    /// 打开表单时的初始值快照：关闭前比对判断是否有未保存修改（脏保护）
    initial: FormSnapshot,
    _subscriptions: Vec<Subscription>,
}

/// 表单可编辑项的值快照（脏检测用；输入框取字符串值，开关取枚举/布尔）
#[derive(PartialEq)]
struct FormSnapshot {
    driver_id: &'static str,
    fields: Vec<String>,
    production: bool,
    tls: bool,
    tls_verify: ramag_domain::entities::TlsVerify,
}

impl EventEmitter<FormEvent> for ConnectionFormPanel {}

impl ConnectionFormPanel {
    pub fn new_create(
        service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(
            service,
            redis_service,
            mongo_service,
            FormMode::Create,
            None,
            window,
            cx,
        )
    }

    pub fn new_edit(
        service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        existing: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mode = FormMode::Edit(existing.id.clone());
        Self::build(
            service,
            redis_service,
            mongo_service,
            mode,
            Some(existing),
            window,
            cx,
        )
    }

    fn build(
        service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        mode: FormMode,
        prefill: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 新建：输入框留空，默认值以 placeholder 虚影呈现（保存留空回退，见 ops::validate）
        // 编辑：回填已有连接的真实值
        let is_create = prefill.is_none();
        let p = prefill.unwrap_or_else(|| ConnectionConfig {
            id: ConnectionId::new(),
            name: String::new(),
            driver: DriverKind::Mysql,
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            database: None,
            auth_source: None,
            remark: None,
            production: false,
            tls: false,
            tls_verify: Default::default(),
            ca_cert_path: None,
            ssh_target: None,
            ssh_port: None,
        });
        let driver_id = driver_kind_to_id(p.driver);
        let remark = p.remark.clone();
        let port_text = if is_create {
            String::new()
        } else {
            p.port.to_string()
        };
        // 名称留空时保存即用 Host 作为连接名，虚影同步显示这一默认
        let name_placeholder = if p.host.is_empty() {
            defaults::DEFAULT_HOST.to_string()
        } else {
            p.host.clone()
        };

        let name = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_NAME_BYTES, window, cx)
                .placeholder(name_placeholder)
                .default_value(p.name)
        });
        let host = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_HOST_BYTES, window, cx)
                .placeholder(defaults::DEFAULT_HOST)
                .default_value(p.host)
        });
        let port = cx.new(|cx| {
            bounded_input(MAX_PORT_TEXT_BYTES, window, cx)
                .placeholder(defaults::default_port(driver_id).to_string())
                .default_value(port_text)
        });
        let username = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_IDENTIFIER_BYTES, window, cx)
                .placeholder(defaults::username_placeholder(driver_id))
                .default_value(p.username)
        });
        let password = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_PASSWORD_BYTES, window, cx)
                .placeholder("（留空表示无密码）")
                .masked(true)
                .default_value(p.password)
        });
        let database = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_IDENTIFIER_BYTES, window, cx)
                .placeholder(defaults::database_placeholder(driver_id))
                .default_value(p.database.unwrap_or_default())
        });
        // 认证库（authSource）：仅 MongoDB 渲染，虚影提示默认 admin
        let auth_source = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_IDENTIFIER_BYTES, window, cx)
                .placeholder("admin")
                .default_value(p.auth_source.unwrap_or_default())
        });
        // 自定义 CA 证书路径（仅 TLS 开启时渲染；留空用系统信任链）
        let ca_cert_path = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_PATH_BYTES, window, cx)
                .placeholder("CA 证书路径（PEM，可选；留空用系统信任链）")
                .default_value(p.ca_cert_path.unwrap_or_default())
        });
        // MongoDB URI 粘贴框（仅 MongoDB 渲染，粘贴后点「填充」解析回填表单）
        let mongo_uri = cx.new(|cx| {
            bounded_input(uri::MAX_MONGO_URI_BYTES, window, cx)
                .placeholder("mongodb://user:pass@host:27017/db?tls=true")
        });
        // SSH 跳板：target 非空即启用；认证走系统 ssh（密钥 / agent / config 别名）
        let ssh_target = cx.new(|cx| {
            bounded_input(MAX_CONNECTION_SSH_TARGET_BYTES, window, cx)
                .placeholder("user@bastion 或 ~/.ssh/config 别名（留空不启用）")
                .default_value(p.ssh_target.unwrap_or_default())
        });
        let ssh_port = cx.new(|cx| {
            bounded_input(MAX_PORT_TEXT_BYTES, window, cx)
                .placeholder("22")
                .default_value(p.ssh_port.map(|v| v.to_string()).unwrap_or_default())
        });

        // host 变化 → 名称虚影跟随（名称始终留给用户输入，不写入真实值）
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &host,
            window,
            |this: &mut Self, _, _e: &InputEvent, window, cx| {
                let host_val = this.host.read(cx).value().trim().to_string();
                let preview = if host_val.is_empty() {
                    defaults::DEFAULT_HOST.to_string()
                } else {
                    host_val
                };
                this.name.update(cx, |state, cx| {
                    state.set_placeholder(preview, window, cx);
                });
            },
        ));

        // 连接参数变化 → 重置已显示的测试结论（旧结论对新参数不成立）；名称/颜色不影响连通性
        for input in [
            &host,
            &port,
            &username,
            &password,
            &database,
            &auth_source,
            &ca_cert_path,
            &ssh_target,
            &ssh_port,
        ] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this: &mut Self, _, e: &InputEvent, _, cx| {
                    if matches!(e, InputEvent::Change) {
                        this.invalidate_test(cx);
                    }
                },
            ));
        }

        let initial_production = p.production;
        let initial_tls = p.tls;
        let initial_tls_verify = p.tls_verify;

        let mut this = Self {
            service,
            redis_service,
            mongo_service,
            mode,
            driver_id,
            name,
            host,
            port,
            username,
            password,
            password_masked: true,
            database,
            auth_source,
            remark,
            production: initial_production,
            tls: initial_tls,
            tls_verify: initial_tls_verify,
            ca_cert_path,
            mongo_uri,
            ssh_target,
            ssh_port,
            test_state: TestState::Idle,
            test_epoch: 0,
            saving: false,
            initial: FormSnapshot {
                driver_id,
                fields: Vec::new(),
                production: initial_production,
                tls: initial_tls,
                tls_verify: initial_tls_verify,
            },
            _subscriptions: subscriptions,
        };
        // 快照要读输入框真实值（default_value 已写入），struct 建好后再取一次
        this.initial = this.snapshot(cx);
        this
    }

    /// 当前表单值快照（与 initial 比对做脏检测）
    fn snapshot(&self, cx: &gpui::App) -> FormSnapshot {
        let fields = [
            &self.name,
            &self.host,
            &self.port,
            &self.username,
            &self.password,
            &self.database,
            &self.auth_source,
            &self.ca_cert_path,
            &self.mongo_uri,
            &self.ssh_target,
            &self.ssh_port,
        ]
        .iter()
        .map(|input| input.read(cx).value().to_string())
        .collect();
        FormSnapshot {
            driver_id: self.driver_id,
            fields,
            production: self.production,
            tls: self.tls,
            tls_verify: self.tls_verify,
        }
    }

    /// 是否有未保存的修改（关闭前脏保护判定）
    pub fn is_dirty(&self, cx: &gpui::App) -> bool {
        self.snapshot(cx) != self.initial
    }

    pub fn is_saving(&self) -> bool {
        self.saving
    }
}

/// 在调用方使用：根据 mode 计算 dialog 标题（不显示具体 driver，已由表单内 driver 选择行体现）
pub fn dialog_title(mode: &FormMode) -> &'static str {
    match mode {
        FormMode::Create => "新建连接",
        FormMode::Edit(_) => "编辑连接",
    }
}

impl ConnectionFormPanel {
    /// 公开 mode 给 dialog 标题使用
    pub fn mode(&self) -> &FormMode {
        &self.mode
    }
}

pub(super) fn section_title(text: &str, muted_fg: gpui::Hsla) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(muted_fg)
                .child(text.to_string()),
        )
        .child(div().flex_1().h(px(1.0)).bg(muted_fg).opacity(0.12))
}

pub(super) fn field_row(label: &str, input: Input) -> impl IntoElement {
    v_flex()
        .gap(px(6.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .child(div().w_full().child(input))
}

/// DriverKind → driver_id 字符串（用于 UI 选择器内部状态）
fn driver_kind_to_id(kind: DriverKind) -> &'static str {
    match kind {
        DriverKind::Mysql => "mysql",
        DriverKind::Postgres => "postgres",
        DriverKind::Redis => "redis",
        DriverKind::Mongodb => "mongodb",
    }
}

/// driver_id → DriverKind；不可用 / 未来 driver 返回 None
fn id_to_driver_kind(id: &str) -> Option<DriverKind> {
    match id {
        "mysql" => Some(DriverKind::Mysql),
        "postgres" => Some(DriverKind::Postgres),
        "redis" => Some(DriverKind::Redis),
        "mongodb" => Some(DriverKind::Mongodb),
        _ => None,
    }
}

// 注：ConnectionFormPanel 没有提供 cx.new 工厂函数，因为 InputState 需要 &mut Window，
// 而 cx.new 的闭包只能拿到 Context。调用方必须在持有 Window 的上下文里直接调用：
//   `cx.new(|cx| ConnectionFormPanel::new_create(svc, window, cx))`

mod defaults;
mod ops;
mod render;
mod uri;
