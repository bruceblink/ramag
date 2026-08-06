//! JumpServer 已保存连接与资源目录加载。

use std::sync::Arc;

use gpui::{Context, Window};
use ramag_domain::entities::JumpServerCredential;

use super::jumpserver_dialog::{JumpServerOperation, JumpServerPanel, JumpServerTreeSelection};

impl JumpServerPanel {
    pub(super) fn restore_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let result = service.load_jumpserver_connections().await;
            let _ = this.update_in(async_cx, |this, _window, cx| {
                this.operation = None;
                match result {
                    Ok(connections) => {
                        this.connections = Arc::new(connections);
                        this.editing_connection = false;
                        if let Some(connection) = this.connections.first().cloned() {
                            this.selected_connection_id = Some(connection.id);
                            this.load_assets(cx);
                        }
                    }
                    Err(error) => {
                        this.notify_error(format!(
                            "读取已保存的 JumpServer 连接失败：{}",
                            error.message()
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_connection(
        &mut self,
        connection_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() {
            return;
        }
        if self.selected_connection_id.as_deref() == Some(&connection_id) {
            self.editing_connection = false;
            self.invalidate_loaded_assets();
            self.load_assets(cx);
            return;
        }
        let Some(connection) = self
            .connections
            .iter()
            .find(|connection| connection.id == connection_id)
            .cloned()
        else {
            self.notify_error("选中的 JumpServer 连接已不存在");
            cx.notify();
            return;
        };
        self.selected_connection_id = Some(connection.id);
        self.editing_connection = false;
        self.invalidate_loaded_assets();
        self.load_assets(cx);
    }

    pub(super) fn new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.selected_connection_id = None;
        self.editing_connection = false;
        self.invalidate_loaded_assets();
        let credential = JumpServerCredential {
            base_url: String::new(),
            ssh_port: 2222,
            username: String::new(),
            password: String::new(),
        };
        self.fill_credential(&credential, window, cx);
        cx.notify();
    }

    pub(super) fn edit_connection(
        &mut self,
        connection_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() {
            return;
        }
        let Some(connection) = self
            .connections
            .iter()
            .find(|connection| connection.id == connection_id)
            .cloned()
        else {
            self.notify_error("选中的 JumpServer 连接已不存在");
            cx.notify();
            return;
        };
        self.selected_connection_id = Some(connection.id);
        self.editing_connection = true;
        self.invalidate_loaded_assets();
        self.fill_credential(&connection.credential, window, cx);
        cx.notify();
    }

    pub(super) fn cancel_edit_connection(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() || !self.editing_connection {
            return;
        }
        self.editing_connection = false;
        self.load_assets(cx);
    }

    fn fill_credential(
        &mut self,
        credential: &JumpServerCredential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (input, value) in [
            (&self.base_url, credential.base_url.clone()),
            (&self.ssh_port, credential.ssh_port.to_string()),
            (&self.username, credential.username.clone()),
            (&self.password, credential.password.clone()),
        ] {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    fn credential(&self, cx: &gpui::App) -> Result<JumpServerCredential, String> {
        let raw_port = self.ssh_port.read(cx).value().trim().to_string();
        let ssh_port = raw_port
            .parse::<u16>()
            .map_err(|_| "SSH 端口必须是 1 - 65535".to_string())?;
        let credential = JumpServerCredential {
            base_url: self.base_url.read(cx).value().trim().to_string(),
            ssh_port,
            username: self.username.read(cx).value().trim().to_string(),
            password: self.password.read(cx).value().to_string(),
        };
        credential.validate()?;
        Ok(credential)
    }

    pub(super) fn test_new_connection(&mut self, cx: &mut Context<Self>) {
        self.run_new_connection_operation(JumpServerOperation::TestingConnection, cx);
    }

    pub(super) fn save_new_connection(&mut self, cx: &mut Context<Self>) {
        self.run_new_connection_operation(JumpServerOperation::SavingConnection, cx);
    }

    fn run_new_connection_operation(
        &mut self,
        operation: JumpServerOperation,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || (self.selected_connection_id.is_some() && !self.editing_connection) {
            return;
        }
        let credential = match self.credential(cx) {
            Ok(credential) => credential,
            Err(message) => {
                self.notify_error(message);
                cx.notify();
                return;
            }
        };
        self.operation = Some(operation);
        let service = self.service.clone();
        let connection_id = self.selected_connection_id.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                let session = service.authenticate_jumpserver(&credential).await?;
                if operation == JumpServerOperation::SavingConnection {
                    let canonical = JumpServerCredential {
                        base_url: session.base_url,
                        ssh_port: session.ssh_port,
                        username: session.username,
                        password: session.password,
                    };
                    let connection = service
                        .save_jumpserver_connection(connection_id.as_deref(), &canonical)
                        .await?;
                    let connections = service.load_jumpserver_connections().await?;
                    Ok(Some((connection, connections)))
                } else {
                    Ok::<_, ramag_domain::error::DomainError>(None)
                }
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.operation = None;
                match result {
                    Ok(Some((connection, connections))) => {
                        this.connections = Arc::new(connections);
                        this.selected_connection_id = Some(connection.id);
                        this.editing_connection = false;
                        this.notify_success("JumpServer 连接已加密保存");
                        this.load_assets(cx);
                    }
                    Ok(None) => {
                        this.notify_success("JumpServer 登录成功");
                    }
                    Err(error) => {
                        let action = if operation == JumpServerOperation::TestingConnection {
                            "测试连接"
                        } else {
                            "保存连接"
                        };
                        this.notify_error(format!("{action}失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn load_assets(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let Some(connection) = self
            .selected_connection_id
            .as_ref()
            .and_then(|connection_id| {
                self.connections
                    .iter()
                    .find(|connection| &connection.id == connection_id)
            })
            .cloned()
        else {
            self.notify_error("请先选择一个已保存的 JumpServer 连接");
            cx.notify();
            return;
        };
        self.invalidate_loaded_assets();
        let generation = self.generation;
        self.operation = Some(JumpServerOperation::LoadingAssets);
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                let session = service
                    .authenticate_jumpserver(&connection.credential)
                    .await?;
                let catalog = service.load_jumpserver_catalog(&session).await?;
                Ok::<_, ramag_domain::error::DomainError>((session, catalog))
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation = None;
                match result {
                    Ok((session, catalog)) => {
                        let is_empty = catalog.assets.is_empty();
                        this.session = Some(session);
                        this.apply_catalog(catalog);
                        if is_empty {
                            this.notify_info("未找到当前用户可访问的资源");
                        }
                    }
                    Err(error) => {
                        this.notify_error(format!("获取失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn invalidate_loaded_assets(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.session = None;
        self.assets = Arc::new(Vec::new());
        self.nodes = Arc::new(Vec::new());
        self.expanded_tree_items.clear();
        self.selected_tree_item = JumpServerTreeSelection::All;
        self.selected_asset_id = None;
        self.detail = None;
        self.detail_error = None;
        self.selected_account_id = None;
        self.saved_selections.clear();
    }

    pub(super) fn toggle_password_mask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.password_masked = !self.password_masked;
        let masked = self.password_masked;
        self.password
            .update(cx, |state, cx| state.set_masked(masked, window, cx));
        cx.notify();
    }

    pub(super) fn request_delete_connection(
        &mut self,
        connection_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() {
            return;
        }
        let Some(connection) = self
            .connections
            .iter()
            .find(|connection| connection.id == connection_id)
        else {
            self.notify_error("选中的 JumpServer 连接已不存在");
            cx.notify();
            return;
        };
        let description = format!(
            "将删除 {} @ {} 的本机加密登录信息，不影响 JumpServer 服务端。依赖此登录信息的远程会话收藏和历史将无法再次打开，但记录仍会保留。",
            connection.credential.username, connection.credential.base_url
        );
        let panel = cx.entity().clone();
        ramag_ui::open_confirm(
            "删除 JumpServer 连接？",
            description,
            "删除",
            true,
            move |window, app| {
                panel.update(app, |this, cx| {
                    this.delete_connection(connection_id.clone(), window, cx);
                });
            },
            window,
            cx,
        );
    }

    fn delete_connection(
        &mut self,
        connection_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation = Some(JumpServerOperation::LoadingConnections);
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let result = async {
                service.delete_jumpserver_connection(&connection_id).await?;
                service.load_jumpserver_connections().await
            }
            .await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                this.operation = None;
                match result {
                    Ok(connections) => {
                        this.connections = Arc::new(connections);
                        if this.selected_connection_id.as_deref() == Some(&connection_id) {
                            if let Some(connection) = this.connections.first().cloned() {
                                this.selected_connection_id = Some(connection.id);
                                this.editing_connection = false;
                                this.invalidate_loaded_assets();
                                this.load_assets(cx);
                            } else {
                                this.new_connection(window, cx);
                            }
                        }
                        this.notify_success("已删除 JumpServer 连接");
                    }
                    Err(error) => {
                        this.notify_error(format!("删除连接失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
