//! JumpServer 登录、资源选择及测试/保存状态。

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use ramag_app::SshService;
use ramag_domain::entities::{
    JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog, JumpServerConnection,
    JumpServerNode, JumpServerSession, MAX_JUMPSERVER_URL_BYTES, MAX_SSH_PASSWORD_BYTES,
    MAX_SSH_USERNAME_BYTES, SshProfile, contains_case_insensitive,
};

#[derive(Debug, Clone)]
pub(super) enum JumpServerEvent {
    Saved(Box<SshProfile>),
}

impl EventEmitter<JumpServerEvent> for JumpServerPanel {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpServerOperation {
    LoadingConnections,
    TestingConnection,
    SavingConnection,
    LoadingAssets,
    LoadingDetail,
    Testing,
    Saving,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JumpServerTreeSelection {
    All,
    Organization(String),
    Node { org_id: String, node_id: String },
}

pub(super) struct JumpServerPanel {
    pub(super) service: Arc<SshService>,
    pub(super) base_url: Entity<InputState>,
    pub(super) ssh_port: Entity<InputState>,
    pub(super) username: Entity<InputState>,
    pub(super) password: Entity<InputState>,
    pub(super) search: Entity<InputState>,
    pub(super) password_masked: bool,
    pub(super) connections: Arc<Vec<JumpServerConnection>>,
    pub(super) selected_connection_id: Option<String>,
    pub(super) editing_connection: bool,
    pub(super) session: Option<JumpServerSession>,
    pub(super) assets: Arc<Vec<JumpServerAsset>>,
    pub(super) nodes: Arc<Vec<JumpServerNode>>,
    pub(super) expanded_tree_items: HashSet<String>,
    pub(super) selected_tree_item: JumpServerTreeSelection,
    pub(super) query: String,
    pub(super) selected_asset_id: Option<String>,
    pub(super) detail: Option<JumpServerAssetDetail>,
    pub(super) detail_error: Option<String>,
    pub(super) selected_account_id: Option<String>,
    pub(super) saved_selections: HashSet<(String, String)>,
    pub(super) operation: Option<JumpServerOperation>,
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    pub(super) generation: u64,
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
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("搜索资源（名称 / 地址 / 平台）")
        });

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

        let mut this = Self {
            service,
            base_url,
            ssh_port,
            username,
            password,
            search,
            password_masked: true,
            connections: Arc::new(Vec::new()),
            selected_connection_id: None,
            editing_connection: false,
            session: None,
            assets: Arc::new(Vec::new()),
            nodes: Arc::new(Vec::new()),
            expanded_tree_items: HashSet::new(),
            selected_tree_item: JumpServerTreeSelection::All,
            query: String::new(),
            selected_asset_id: None,
            detail: None,
            detail_error: None,
            selected_account_id: None,
            saved_selections: HashSet::new(),
            operation: Some(JumpServerOperation::LoadingConnections),
            pending_notification: None,
            generation: 0,
            _subscriptions: subscriptions,
        };
        this.restore_connections(window, cx);
        this
    }

    pub(super) fn is_busy(&self) -> bool {
        self.operation.is_some()
    }

    pub(super) fn notify_success(&mut self, message: impl Into<String>) {
        self.pending_notification = Some(
            gpui_component::notification::Notification::success(message.into()).autohide(true),
        );
    }

    pub(super) fn notify_error(&mut self, message: impl Into<String>) {
        self.pending_notification = Some(gpui_component::notification::Notification::error(
            message.into(),
        ));
    }

    pub(super) fn notify_info(&mut self, message: impl Into<String>) {
        self.pending_notification =
            Some(gpui_component::notification::Notification::info(message.into()).autohide(true));
    }

    pub(super) fn filtered_assets(&self) -> Vec<JumpServerAsset> {
        let selected_node = match &self.selected_tree_item {
            JumpServerTreeSelection::Node { org_id, node_id } => self
                .nodes
                .iter()
                .find(|node| &node.org_id == org_id && &node.id == node_id),
            _ => None,
        };
        let descendant_ids = selected_node
            .filter(|node| !node.is_special())
            .map(|selected| {
                self.nodes
                    .iter()
                    .filter(|node| {
                        node.org_id == selected.org_id
                            && (node.key == selected.key
                                || node.key.starts_with(&format!("{}:", selected.key)))
                    })
                    .map(|node| node.id.as_str())
                    .collect::<HashSet<_>>()
            });
        self.assets
            .iter()
            .filter(|asset| {
                let in_selected_tree = match &self.selected_tree_item {
                    JumpServerTreeSelection::All => true,
                    JumpServerTreeSelection::Organization(org_id) => &asset.org_id == org_id,
                    JumpServerTreeSelection::Node { org_id, .. } => {
                        &asset.org_id == org_id
                            && selected_node.is_some_and(|node| {
                                if node.is_favorite() {
                                    asset.favorite
                                } else if node.is_ungrouped() {
                                    asset.ungrouped
                                } else {
                                    descendant_ids.as_ref().is_some_and(|ids| {
                                        asset.node_ids.iter().any(|id| ids.contains(id.as_str()))
                                    })
                                }
                            })
                    }
                };
                in_selected_tree
                    && (self.query.is_empty()
                        || contains_case_insensitive(&asset.name, &self.query)
                        || contains_case_insensitive(&asset.address, &self.query)
                        || contains_case_insensitive(&asset.platform, &self.query)
                        || asset.labels.iter().any(|label| {
                            contains_case_insensitive(&label.name, &self.query)
                                || contains_case_insensitive(&label.value, &self.query)
                        }))
            })
            .cloned()
            .collect()
    }

    pub(super) fn apply_catalog(&mut self, catalog: JumpServerCatalog) {
        self.nodes = Arc::new(catalog.nodes);
        self.assets = Arc::new(catalog.assets);
        self.expanded_tree_items.clear();

        let organization_ids = self
            .assets
            .iter()
            .map(|asset| asset.org_id.as_str())
            .collect::<HashSet<_>>();
        if organization_ids.len() > 1 {
            for org_id in organization_ids {
                self.expanded_tree_items
                    .insert(tree_organization_identity(org_id));
            }
            self.selected_tree_item = JumpServerTreeSelection::All;
            return;
        }

        let organization_id = organization_ids.iter().next().copied();
        let selected = self
            .nodes
            .iter()
            .filter(|node| organization_id.is_none_or(|id| node.org_id == id))
            .find(|node| !node.is_special() && node_is_root(&self.nodes, node))
            .or_else(|| {
                self.nodes
                    .iter()
                    .find(|node| organization_id.is_none_or(|id| node.org_id == id))
            });
        if let Some(node) = selected {
            if !node.is_special() {
                self.expanded_tree_items
                    .insert(tree_node_identity(&node.org_id, &node.id));
            }
            self.selected_tree_item = JumpServerTreeSelection::Node {
                org_id: node.org_id.clone(),
                node_id: node.id.clone(),
            };
        } else {
            self.selected_tree_item = JumpServerTreeSelection::All;
        }
    }

    pub(super) fn toggle_tree_node(
        &mut self,
        org_id: String,
        node_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() {
            return;
        }
        let identity = tree_node_identity(&org_id, &node_id);
        if !self.expanded_tree_items.remove(&identity) {
            self.expanded_tree_items.insert(identity);
        }
        cx.notify();
    }

    pub(super) fn toggle_tree_organization(&mut self, org_id: String, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let identity = tree_organization_identity(&org_id);
        if !self.expanded_tree_items.remove(&identity) {
            self.expanded_tree_items.insert(identity);
        }
        cx.notify();
    }

    pub(super) fn select_tree_node(
        &mut self,
        org_id: String,
        node_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy()
            || self.selected_tree_item
                == (JumpServerTreeSelection::Node {
                    org_id: org_id.clone(),
                    node_id: node_id.clone(),
                })
        {
            return;
        }
        self.selected_tree_item = JumpServerTreeSelection::Node { org_id, node_id };
        self.clear_asset_selection();
        cx.notify();
    }

    pub(super) fn select_tree_organization(&mut self, org_id: String, cx: &mut Context<Self>) {
        if self.is_busy()
            || self.selected_tree_item == JumpServerTreeSelection::Organization(org_id.clone())
        {
            return;
        }
        self.selected_tree_item = JumpServerTreeSelection::Organization(org_id);
        self.clear_asset_selection();
        cx.notify();
    }

    pub(super) fn select_all_assets(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() || self.selected_tree_item == JumpServerTreeSelection::All {
            return;
        }
        self.selected_tree_item = JumpServerTreeSelection::All;
        self.clear_asset_selection();
        cx.notify();
    }

    pub(super) fn selected_tree_name(&self) -> String {
        match &self.selected_tree_item {
            JumpServerTreeSelection::All => "全部资源".into(),
            JumpServerTreeSelection::Organization(org_id) => self
                .session
                .as_ref()
                .and_then(|session| {
                    session
                        .organizations
                        .iter()
                        .find(|organization| &organization.id == org_id)
                })
                .map_or_else(
                    || "全部资源".into(),
                    |organization| organization.name.clone(),
                ),
            JumpServerTreeSelection::Node { org_id, node_id } => self
                .nodes
                .iter()
                .find(|node| &node.org_id == org_id && &node.id == node_id)
                .map_or_else(|| "全部资源".into(), |node| node.name.clone()),
        }
    }

    fn clear_asset_selection(&mut self) {
        self.selected_asset_id = None;
        self.detail = None;
        self.detail_error = None;
        self.selected_account_id = None;
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
        self.nodes = Arc::new(Vec::new());
        self.expanded_tree_items.clear();
        self.selected_tree_item = JumpServerTreeSelection::All;
        self.selected_asset_id = None;
        self.detail = None;
        self.detail_error = None;
        self.selected_account_id = None;
        self.saved_selections.clear();
        cx.notify();
    }
}

pub(super) fn tree_node_identity(org_id: &str, node_id: &str) -> String {
    format!("node:{org_id}:{node_id}")
}

pub(super) fn tree_organization_identity(org_id: &str) -> String {
    format!("organization:{org_id}")
}

pub(super) fn node_is_root(nodes: &[JumpServerNode], node: &JumpServerNode) -> bool {
    node.is_special()
        || !nodes
            .iter()
            .any(|candidate| candidate.org_id == node.org_id && candidate.key == node.parent_key())
}

pub(super) fn detail_unavailable_message(detail: &JumpServerAssetDetail) -> Option<String> {
    if !detail.ssh_enabled {
        return Some(
            "该资源未开放 SSH 协议（可能仅支持 RDP 等其他协议），无法导入为 SSH 连接。".into(),
        );
    }
    if detail
        .accounts
        .iter()
        .any(|account| account.usable_for_direct_login())
    {
        return None;
    }
    if detail.accounts.is_empty() {
        return Some(
            "JumpServer 没有返回该资源的授权账号；请让管理员为当前用户授权资产账号。".into(),
        );
    }
    if detail.accounts.iter().all(|account| !account.can_connect) {
        return Some(format!(
            "已返回 {} 个授权账号，但都缺少 connect 权限；请让管理员补充“连接”权限。",
            detail.accounts.len()
        ));
    }
    Some("授权账号名称不符合 SSH 直连要求，不能包含空白、# 或 @。".into())
}

fn bounded_input(
    max_bytes: usize,
    window: &mut Window,
    cx: &mut Context<InputState>,
) -> InputState {
    InputState::new(window, cx).validate(move |value, _| value.len() <= max_bytes)
}
