//! ConnectionForm 状态 + 异步：driver 切换 / 校验 / 测试 / 保存。render_driver_selector 也在这里

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, img, prelude::*,
    px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use ramag_domain::entities::{ConnectionConfig, ConnectionId, DriverKind};
use tracing::{error, info};

use super::{
    ConnectionFormPanel, DRIVERS, FormEvent, FormMode, TestState, defaults, driver_display_name,
    id_to_driver_kind, section_title,
};

impl ConnectionFormPanel {
    /// 「从 URI 填充」：解析连接地址回填表单各字段；scheme 决定数据库类型。
    /// 新建模式按 scheme 自动切换类型；编辑模式类型固定，URI 不符时显式报错。
    /// 解析失败复用测试结论区显示红字；副本集多主机明确拒绝，避免静默改变拓扑。
    pub(super) fn apply_uri(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let raw = self.uri.read(cx).value().trim().to_string();
        if raw.is_empty() {
            return;
        }
        let parts = match super::uri::parse_connection_uri(&raw) {
            Ok(p) => p,
            Err(msg) => {
                self.test_state = TestState::Failed(format!("URI 解析失败：{msg}"));
                cx.notify();
                return;
            }
        };
        if parts.driver_id != self.driver_id {
            if matches!(self.mode, FormMode::Create) {
                self.set_driver(parts.driver_id, window, cx);
            } else {
                self.test_state = TestState::Failed(format!(
                    "这是 {} 连接地址，与当前连接类型（不可变更）不符",
                    driver_display_name(parts.driver_id)
                ));
                cx.notify();
                return;
            }
        }
        self.host
            .update(cx, |s, cx| s.set_value(parts.host, window, cx));
        self.port.update(cx, |s, cx| {
            let v = parts.port.map(|p| p.to_string()).unwrap_or_default();
            s.set_value(v, window, cx);
        });
        self.username
            .update(cx, |s, cx| s.set_value(parts.username, window, cx));
        self.password
            .update(cx, |s, cx| s.set_value(parts.password, window, cx));
        self.database.update(cx, |s, cx| {
            s.set_value(parts.database.unwrap_or_default(), window, cx)
        });
        self.auth_source.update(cx, |s, cx| {
            s.set_value(parts.auth_source.unwrap_or_default(), window, cx)
        });
        self.tls = parts.tls;
        info!("connection uri applied to form");
        self.invalidate_test(cx);
        cx.notify();
    }

    /// 连接参数变更后调用：丢弃在途测试的结果，并清掉已显示的测试结论
    pub(super) fn invalidate_test(&mut self, cx: &mut Context<Self>) {
        self.test_epoch = self.test_epoch.wrapping_add(1);
        if !matches!(self.test_state, TestState::Idle) {
            self.test_state = TestState::Idle;
            cx.notify();
        }
    }

    /// 切换 driver：端口未被用户修改（空或仍是旧 driver 默认）时清空，让新 driver 虚影默认值显示
    pub(super) fn set_driver(
        &mut self,
        id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving || self.driver_id == id {
            return;
        }
        let cur_port = self.port.read(cx).value().to_string();
        if !cur_port.is_empty() && cur_port == defaults::default_port(self.driver_id).to_string() {
            self.port
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.driver_id = id;
        self.port.update(cx, |state, cx| {
            state.set_placeholder(defaults::default_port(id).to_string(), window, cx);
        });
        self.username.update(cx, |state, cx| {
            state.set_placeholder(defaults::username_placeholder(id), window, cx);
        });
        self.database.update(cx, |state, cx| {
            state.set_placeholder(defaults::database_placeholder(id), window, cx);
        });
        self.uri.update(cx, |state, cx| {
            state.set_placeholder(defaults::uri_placeholder(id), window, cx);
        });
        self.invalidate_test(cx);
        cx.notify();
    }

    /// 校验表单并返回 ConnectionConfig；留空字段回退到 placeholder 虚影显示的默认值
    pub(super) fn validate(&self, cx: &gpui::App) -> Result<ConnectionConfig, String> {
        let driver =
            id_to_driver_kind(self.driver_id).ok_or_else(|| "请选择数据库类型".to_string())?;

        let mut host = self.host.read(cx).value().trim().to_string();
        if host.is_empty() {
            host = defaults::DEFAULT_HOST.to_string();
        }
        // 名称默认跟随 Host（与名称输入框虚影一致）
        let mut name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            name = host.clone();
        }
        let port_str = self.port.read(cx).value().trim().to_string();
        let port: u16 = if port_str.is_empty() {
            defaults::default_port(self.driver_id)
        } else {
            port_str
                .parse()
                .map_err(|_| "Port 必须是 1 - 65535 的数字".to_string())?
        };
        if port == 0 {
            return Err("Port 必须是 1 - 65535".into());
        }
        // 用户名留空：MySQL/Postgres 回退默认账号；Redis/MongoDB 保持空（无 ACL / 无认证）
        let mut username = self.username.read(cx).value().trim().to_string();
        if username.is_empty() {
            username = defaults::default_username(self.driver_id).to_string();
        }
        let password = self.password.read(cx).value().to_string();
        let database = {
            let v = self.database.read(cx).value().trim().to_string();
            if v.is_empty() { None } else { Some(v) }
        };
        // authSource 仅 MongoDB 有意义（用户凭证所在库）；其它 driver 不存
        let auth_source = if matches!(driver, DriverKind::Mongodb) {
            let v = self.auth_source.read(cx).value().trim().to_string();
            if v.is_empty() { None } else { Some(v) }
        } else {
            None
        };
        // Redis 的 DB 字段限制 0-255 数字
        if matches!(driver, DriverKind::Redis)
            && let Some(ref s) = database
        {
            s.parse::<u8>()
                .map_err(|_| "DB 必须是 0 - 255 的数字（默认 Redis 上限 0-15）".to_string())?;
        }
        // Postgres 必须连接具体 database（不能不指定）
        if matches!(driver, DriverKind::Postgres) && database.is_none() {
            return Err("PostgreSQL 必须填写默认库".into());
        }
        let id = match &self.mode {
            FormMode::Create => ConnectionId::new(),
            FormMode::Edit(id) => id.clone(),
        };

        // CA 路径仅 TLS 开启时保存；关着时留空避免脏值残留
        let ca_cert_path = if self.tls {
            let v = self.ca_cert_path.read(cx).value().trim().to_string();
            if v.is_empty() { None } else { Some(v) }
        } else {
            None
        };
        // 环境标签：仅列表徽章展示；留空不打标
        let environment = {
            let v = self.environment.read(cx).value().trim().to_string();
            if v.is_empty() { None } else { Some(v) }
        };
        // SSH 跳板：target 非空即启用；端口须为数字
        let ssh_target = {
            let v = self.ssh_target.read(cx).value().trim().to_string();
            if v.is_empty() { None } else { Some(v) }
        };
        let ssh_port = {
            let v = self.ssh_port.read(cx).value().trim().to_string();
            parse_optional_ssh_port(&v)?
        };
        // SSH 与 TLS 允许同开（分别保护跳板段与后段链路）；经隧道后主机名校验必败，
        // driver 层自动降级验证等级（MySQL/PG Full→Ca；Redis/Mongo 仅加密），表单有提示

        let config = ConnectionConfig {
            id,
            name,
            driver,
            host,
            port,
            username,
            password,
            database,
            auth_source,
            remark: self.remark.clone(),
            environment,
            production: self.production,
            tls: self.tls,
            tls_verify: self.tls_verify,
            ca_cert_path,
            ssh_target,
            ssh_port,
        };
        config.validate()?;
        Ok(config)
    }

    /// 渲染 driver 选择器：按钮横排，仅可用 driver 可点
    pub(super) fn render_driver_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let accent = theme.accent;
        let border = theme.border;
        let secondary_bg = theme.secondary;

        let mut accent_tint = accent;
        accent_tint.a = 0.10;
        let mut accent_border = accent;
        accent_border.a = 0.55;

        let mut row = h_flex().w_full().items_center().gap(px(8.0));
        for &(id, name, available) in DRIVERS {
            let is_selected = self.driver_id == id;
            let btn_id = SharedString::from(format!("driver-btn-{id}"));

            // 按钮等分宽度（flex_1 + min_w_0），避免文字长的 PostgreSQL 撑破布局
            let mut btn = h_flex()
                .id(btn_id)
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(7.0))
                .rounded_md()
                .border_1()
                .text_sm();
            // 品牌彩色 logo（img 渲染保留原色，禁用态由按钮整体 opacity 一并变暗）+ 名称
            if let Some(icon) = ramag_ui::icons::db_brand_icon(id) {
                btn = btn.child(img(icon).size(px(16.0)).flex_none());
            }
            btn = btn.child(name.to_string());

            if is_selected {
                // 选中态：accent 描边 + 浅 accent 底
                btn = btn
                    .bg(accent_tint)
                    .border_color(accent_border)
                    .text_color(accent);
            } else if available && !self.saving {
                // 可点击未选中
                btn = btn
                    .bg(secondary_bg)
                    .border_color(border)
                    .text_color(fg)
                    .cursor_pointer()
                    .hover(move |this| this.border_color(accent_border))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_driver(id, window, cx);
                    }));
            } else {
                // 禁用：dim、不可点
                btn = btn
                    .bg(secondary_bg)
                    .border_color(border)
                    .text_color(muted_fg)
                    .opacity(0.45);
            }

            row = row.child(btn);
        }

        v_flex()
            .gap(px(8.0))
            .child(section_title("数据库类型", muted_fg))
            .child(row)
    }

    pub(super) fn handle_test(&mut self, cx: &mut Context<Self>) {
        // 防重入：测试进行中不再发起（结果竞态已由 epoch 挡，这里挡资源重复占用）
        if self.saving || matches!(self.test_state, TestState::Testing) {
            return;
        }
        let config = match self.validate(cx) {
            Ok(c) => c,
            Err(e) => {
                self.test_state = TestState::Failed(e);
                cx.notify();
                return;
            }
        };
        // 测试一律用一次性 id 的副本：池 / SSH 隧道按 ConnectionId 缓存——
        // ① 不污染正式 id 的缓存（Edit 模式下否则会复用旧参数的池，测的不是新参数）
        // ② 测完立即释放，不留孤儿池 / 隧道
        let mut config = config;
        config.id = ConnectionId::new();
        self.test_state = TestState::Testing;
        let epoch = self.test_epoch;
        cx.notify();

        // 按 driver 走对应的 service.test：SQL 类（MySQL/Postgres）→ ConnectionService；Redis → RedisService；MongoDB → MongoService
        let sql_svc = self.service.clone();
        let redis_svc = self.redis_service.clone();
        let mongo_svc = self.mongo_service.clone();
        cx.spawn(async move |this, cx| {
            let result = match config.driver {
                DriverKind::Mysql | DriverKind::Postgres => sql_svc.test(&config).await,
                DriverKind::Redis => redis_svc.test(&config).await,
                DriverKind::Mongodb => mongo_svc.test(&config).await,
            };
            // 无论成败，释放本次测试建的池与 SSH 隧道（一次性 id，无人复用）
            match config.driver {
                DriverKind::Mysql | DriverKind::Postgres => sql_svc.evict_pool(&config),
                DriverKind::Redis => redis_svc.evict_pool(&config.id),
                DriverKind::Mongodb => mongo_svc.evict_pool(&config.id),
            }
            let _ = this.update(cx, |this, cx| {
                // 测试期间参数已变更：结果作废，保持重置后的 Idle
                if this.test_epoch != epoch {
                    return;
                }
                this.test_state = match result {
                    Ok(_) => {
                        info!("test_connection ok");
                        TestState::Success
                    }
                    Err(e) => {
                        error!(error = %e, "test_connection failed");
                        TestState::Failed(e.to_string())
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn handle_save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let config = match self.validate(cx) {
            Ok(c) => c,
            Err(e) => {
                self.test_state = TestState::Failed(e);
                cx.notify();
                return;
            }
        };
        self.saving = true;
        cx.notify();

        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.save(&config).await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(_) => {
                        info!(name = %config.name, "connection saved");
                        cx.emit(FormEvent::Saved(Box::new(config)));
                    }
                    Err(e) => {
                        error!(error = %e, "save connection failed");
                        this.test_state = TestState::Failed(format!("保存失败：{e}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 「取消」按钮：有未保存修改先确认，确认「放弃修改」才发 Cancelled 关闭表单
    pub(super) fn handle_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if !self.is_dirty(cx) {
            cx.emit(FormEvent::Cancelled);
            return;
        }
        let entity = cx.entity();
        ramag_ui::open_confirm(
            "放弃修改？",
            "表单有未保存的修改，关闭将丢弃这些修改。",
            "放弃修改",
            true,
            move |_, app| {
                entity.update(app, |_this, cx| cx.emit(FormEvent::Cancelled));
            },
            window,
            cx,
        );
    }
}

fn parse_optional_ssh_port(raw: &str) -> Result<Option<u16>, String> {
    if raw.is_empty() {
        return Ok(None);
    }
    let port = raw
        .parse::<u16>()
        .map_err(|_| "SSH 端口必须是 1-65535 的数字".to_string())?;
    if port == 0 {
        return Err("SSH 端口必须是 1-65535 的数字".into());
    }
    Ok(Some(port))
}

#[cfg(test)]
mod tests {
    use super::parse_optional_ssh_port;

    #[test]
    fn ssh_port_rejects_zero_and_out_of_range() {
        assert_eq!(parse_optional_ssh_port(""), Ok(None));
        assert_eq!(parse_optional_ssh_port("22"), Ok(Some(22)));
        assert!(parse_optional_ssh_port("0").is_err());
        assert!(parse_optional_ssh_port("65536").is_err());
        assert!(parse_optional_ssh_port("abc").is_err());
    }
}
