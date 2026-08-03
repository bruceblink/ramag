//! JumpServer 登录、资源选择及测试/保存状态。

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use ramag_app::SshService;
use ramag_domain::entities::{
    JumpServerAsset, JumpServerAssetDetail, JumpServerCredential, JumpServerSession,
    MAX_JUMPSERVER_URL_BYTES, MAX_SSH_PASSWORD_BYTES, MAX_SSH_USERNAME_BYTES, SshProfile,
};

#[derive(Debug, Clone)]
pub(super) enum JumpServerEvent {
    Saved(Box<SshProfile>),
    Cancelled,
}

impl EventEmitter<JumpServerEvent> for JumpServerPanel {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpServerOperation {
    LoadingAssets,
    LoadingDetail,
    Testing,
    Saving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpServerFeedbackKind {
    Info,
    Success,
    Error,
}

pub(super) struct JumpServerFeedback {
    pub message: String,
    pub kind: JumpServerFeedbackKind,
}

pub(super) struct JumpServerPanel {
    pub(super) service: Arc<SshService>,
    pub(super) base_url: Entity<InputState>,
    pub(super) ssh_port: Entity<InputState>,
    pub(super) username: Entity<InputState>,
    pub(super) password: Entity<InputState>,
    pub(super) search: Entity<InputState>,
    pub(super) password_masked: bool,
    pub(super) session: Option<JumpServerSession>,
    pub(super) assets: Arc<Vec<JumpServerAsset>>,
    pub(super) query: String,
    pub(super) selected_asset_id: Option<String>,
    pub(super) detail: Option<JumpServerAssetDetail>,
    pub(super) selected_account_id: Option<String>,
    saved_selections: HashSet<(String, String)>,
    pub(super) operation: Option<JumpServerOperation>,
    pub(super) feedback: Option<JumpServerFeedback>,
    generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl JumpServerPanel {
    pub fn new(service: Arc<SshService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let base_url = cx.new(|cx| {
            bounded_input(MAX_JUMPSERVER_URL_BYTES, window, cx)
                .placeholder("https://jump.example.com")
        });
        let ssh_port = cx.new(|cx| {
            bounded_input(5, window, cx)
                .default_value("2222")
                .placeholder("2222")
        });
        let username = cx.new(|cx| {
            bounded_input(MAX_SSH_USERNAME_BYTES, window, cx).placeholder("JumpServer 用户名")
        });
        let password = cx.new(|cx| {
            bounded_input(MAX_SSH_PASSWORD_BYTES, window, cx)
                .masked(true)
                .placeholder("JumpServer 登录密码")
        });
        let search =
            cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("搜索资源"));

        let mut subscriptions = Vec::new();
        for input in [&base_url, &ssh_port, &username, &password] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.invalidate_login(cx);
                    }
                },
            ));
        }
        subscriptions.push(cx.subscribe_in(
            &search,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = this.search.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            },
        ));

        Self {
            service,
            base_url,
            ssh_port,
            username,
            password,
            search,
            password_masked: true,
            session: None,
            assets: Arc::new(Vec::new()),
            query: String::new(),
            selected_asset_id: None,
            detail: None,
            selected_account_id: None,
            saved_selections: HashSet::new(),
            operation: None,
            feedback: None,
            generation: 0,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn is_busy(&self) -> bool {
        self.operation.is_some()
    }

    pub(super) fn filtered_assets(&self) -> Vec<JumpServerAsset> {
        self.assets
            .iter()
            .filter(|asset| {
                self.query.is_empty()
                    || asset.name.to_lowercase().contains(&self.query)
                    || asset.address.to_lowercase().contains(&self.query)
                    || asset.platform.to_lowercase().contains(&self.query)
            })
            .cloned()
            .collect()
    }

    pub(super) fn selected_is_saved(&self) -> bool {
        self.selected_asset_id
            .as_ref()
            .zip(self.selected_account_id.as_ref())
            .is_some_and(|(asset_id, account_id)| {
                self.saved_selections
                    .contains(&(asset_id.clone(), account_id.clone()))
            })
    }

    fn invalidate_login(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.session = None;
        self.assets = Arc::new(Vec::new());
        self.selected_asset_id = None;
        self.detail = None;
        self.selected_account_id = None;
        self.saved_selections.clear();
        if self.operation.is_none() {
            self.feedback = None;
        }
        cx.notify();
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

    pub(super) fn load_assets(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let credential = match self.credential(cx) {
            Ok(credential) => credential,
            Err(message) => {
                self.feedback = Some(JumpServerFeedback {
                    message,
                    kind: JumpServerFeedbackKind::Error,
                });
                cx.notify();
                return;
            }
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.operation = Some(JumpServerOperation::LoadingAssets);
        self.session = None;
        self.assets = Arc::new(Vec::new());
        self.selected_asset_id = None;
        self.detail = None;
        self.selected_account_id = None;
        self.saved_selections.clear();
        self.feedback = Some(JumpServerFeedback {
            message: "正在登录并获取资源…".into(),
            kind: JumpServerFeedbackKind::Info,
        });
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                let session = service.authenticate_jumpserver(&credential).await?;
                let assets = service.list_jumpserver_assets(&session).await?;
                Ok::<_, ramag_domain::error::DomainError>((session, assets))
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation = None;
                match result {
                    Ok((session, assets)) => {
                        let count = assets.len();
                        this.session = Some(session);
                        this.assets = Arc::new(assets);
                        this.feedback = Some(JumpServerFeedback {
                            message: if count == 0 {
                                "未找到当前用户可访问的资源".into()
                            } else {
                                format!("已获取 {count} 个资源")
                            },
                            kind: if count == 0 {
                                JumpServerFeedbackKind::Info
                            } else {
                                JumpServerFeedbackKind::Success
                            },
                        });
                    }
                    Err(error) => {
                        this.feedback = Some(JumpServerFeedback {
                            message: format!("获取失败：{}", error.message()),
                            kind: JumpServerFeedbackKind::Error,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn select_asset(&mut self, asset_id: String, cx: &mut Context<Self>) {
        if self.is_busy() || self.selected_asset_id.as_deref() == Some(asset_id.as_str()) {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(asset) = self
            .assets
            .iter()
            .find(|asset| asset.id == asset_id && asset.active)
            .cloned()
        else {
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.selected_asset_id = Some(asset.id.clone());
        self.detail = None;
        self.selected_account_id = None;
        self.operation = Some(JumpServerOperation::LoadingDetail);
        self.feedback = Some(JumpServerFeedback {
            message: "正在获取资产账号…".into(),
            kind: JumpServerFeedbackKind::Info,
        });
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.jumpserver_asset_detail(&session, &asset).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation = None;
                match result {
                    Ok(detail) => {
                        this.selected_account_id = detail
                            .accounts
                            .iter()
                            .find(|account| account.usable_for_direct_login())
                            .map(|account| account.id.clone());
                        let has_usable_account = this.selected_account_id.is_some();
                        let ssh_enabled = detail.ssh_enabled;
                        this.detail = Some(detail);
                        this.feedback = if !ssh_enabled {
                            Some(JumpServerFeedback {
                                message: "该资产未开放 SSH 协议".into(),
                                kind: JumpServerFeedbackKind::Error,
                            })
                        } else if !has_usable_account {
                            Some(JumpServerFeedback {
                                message: "该资产没有可连接的授权账号".into(),
                                kind: JumpServerFeedbackKind::Error,
                            })
                        } else {
                            None
                        };
                    }
                    Err(error) => {
                        this.feedback = Some(JumpServerFeedback {
                            message: format!("加载失败：{}", error.message()),
                            kind: JumpServerFeedbackKind::Error,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn select_account(&mut self, account_id: String, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let usable = self.detail.as_ref().is_some_and(|detail| {
            detail
                .accounts
                .iter()
                .any(|account| account.id == account_id && account.usable_for_direct_login())
        });
        if usable {
            self.selected_account_id = Some(account_id);
            self.feedback = None;
            cx.notify();
        }
    }

    pub(super) fn test_selected(&mut self, cx: &mut Context<Self>) {
        self.run_selected_operation(JumpServerOperation::Testing, cx);
    }

    pub(super) fn save_selected(&mut self, cx: &mut Context<Self>) {
        self.run_selected_operation(JumpServerOperation::Saving, cx);
    }

    fn run_selected_operation(&mut self, operation: JumpServerOperation, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let (Some(session), Some(asset_id), Some(account_id)) = (
            self.session.clone(),
            self.selected_asset_id.clone(),
            self.selected_account_id.clone(),
        ) else {
            self.feedback = Some(JumpServerFeedback {
                message: "请先选择资源和资产账号".into(),
                kind: JumpServerFeedbackKind::Error,
            });
            cx.notify();
            return;
        };
        let Some(asset) = self
            .assets
            .iter()
            .find(|asset| asset.id == asset_id)
            .cloned()
        else {
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.operation = Some(operation);
        self.feedback = Some(JumpServerFeedback {
            message: if operation == JumpServerOperation::Testing {
                "正在刷新连接信息并测试…".into()
            } else {
                "正在刷新连接信息并保存…".into()
            },
            kind: JumpServerFeedbackKind::Info,
        });
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = if operation == JumpServerOperation::Testing {
                service
                    .test_jumpserver_asset(&session, &asset, &account_id)
                    .await
            } else {
                service
                    .save_jumpserver_asset(&session, &asset, &account_id)
                    .await
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation = None;
                match result {
                    Ok(profile) if operation == JumpServerOperation::Saving => {
                        this.saved_selections
                            .insert((asset.id.clone(), account_id.clone()));
                        this.feedback = Some(JumpServerFeedback {
                            message: format!("已保存：{}", profile.name),
                            kind: JumpServerFeedbackKind::Success,
                        });
                        cx.emit(JumpServerEvent::Saved(Box::new(profile)));
                    }
                    Ok(_) => {
                        this.feedback = Some(JumpServerFeedback {
                            message: "连接成功".into(),
                            kind: JumpServerFeedbackKind::Success,
                        });
                    }
                    Err(error) => {
                        let action = if operation == JumpServerOperation::Testing {
                            "测试"
                        } else {
                            "保存"
                        };
                        this.feedback = Some(JumpServerFeedback {
                            message: format!("{action}失败：{}", error.message()),
                            kind: JumpServerFeedbackKind::Error,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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

    pub(super) fn request_cancel(&mut self, cx: &mut Context<Self>) {
        if !self.is_busy() {
            cx.emit(JumpServerEvent::Cancelled);
        }
    }
}

fn bounded_input(
    max_bytes: usize,
    window: &mut Window,
    cx: &mut Context<InputState>,
) -> InputState {
    InputState::new(window, cx).validate(move |value, _| value.len() <= max_bytes)
}
