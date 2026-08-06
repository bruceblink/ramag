//! SSH 工具根视图与状态模型。

mod file_chunk;
mod file_preview;
mod file_preview_layout;
mod file_syntax;
mod jumpserver_asset_ops;
mod jumpserver_connection_ops;
mod jumpserver_dialog;
mod model;
mod ops;
mod ops_connection;
mod ops_diagnostic;
mod ops_files;
mod ops_profile;
mod ops_transfer;
mod path_dialog;
mod profile_dialog;
mod profile_form;
mod remote_session_dialog;
mod render;
mod render_diagnostic;
mod render_directory_helpers;
mod render_jumpserver_connections;
mod render_jumpserver_dialog;
mod render_jumpserver_rows;
mod render_jumpserver_tree;
mod render_manager;
mod render_profile_form;
mod render_transfers;
mod render_workspace;
mod ssh_command;
mod terminal_startup;

#[cfg(test)]
mod render_test;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, FocusHandle, Focusable, Subscription, Window};
use gpui_component::{
    input::{InputEvent, InputState},
    resizable::ResizableState,
};
use ramag_app::SshService;
use ramag_domain::entities::{SshCapability, SshProfile, SshProfileId};

use model::{Notice, SshWorkspace, ViewMode};

pub struct SshView {
    service: Arc<SshService>,
    profiles: Arc<Vec<SshProfile>>,
    loading_profiles: bool,
    load_error: Option<String>,
    default_capability: Option<Result<SshCapability, String>>,
    search: Entity<InputState>,
    query: String,
    directory_search: Entity<InputState>,
    diagnostic_search: Entity<InputState>,
    workspace_resizes: HashMap<SshProfileId, Entity<ResizableState>>,
    focused_search_once: bool,
    deleting_profile: bool,
    opening_rdp_profile: Option<SshProfileId>,
    profile_form_subscription: Option<Subscription>,
    jumpserver_subscription: Option<Subscription>,
    notice: Option<Notice>,
    view_mode: ViewMode,
    workspaces: Vec<SshWorkspace>,
    path_favorites: HashMap<SshProfileId, Vec<String>>,
    active_workspace_id: Option<SshProfileId>,
    next_terminal_id: u64,
    load_generation: u64,
    capability_generation: u64,
    persist_generation: u64,
    last_transfer_revision: u64,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SshView {
    pub fn new(service: Arc<SshService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("搜索连接（名称 / 地址 / 用户 / 环境）")
        });
        let directory_search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("搜索名称")
                .clean_on_escape()
        });
        let diagnostic_search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("筛选诊断结果")
                .clean_on_escape()
        });
        let mut subscriptions = Vec::new();
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
        subscriptions.push(cx.subscribe_in(
            &directory_search,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = this.directory_search.read(cx).value().trim().to_lowercase();
                    if let Some(workspace_id) = this.active_workspace_id.clone()
                        && let Some(workspace) = this.workspace_mut(&workspace_id)
                    {
                        workspace.directory_query = query;
                        workspace.selected_path = None;
                    }
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &diagnostic_search,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = this.diagnostic_search.read(cx).value().trim().to_string();
                    if let Some(workspace_id) = this.active_workspace_id.clone()
                        && let Some(workspace) = this.workspace_mut(&workspace_id)
                    {
                        workspace.diagnostic_query = query;
                    }
                    cx.notify();
                }
            },
        ));
        let mut this = Self {
            service,
            profiles: Arc::new(Vec::new()),
            loading_profiles: true,
            load_error: None,
            default_capability: None,
            search,
            query: String::new(),
            directory_search,
            diagnostic_search,
            workspace_resizes: HashMap::new(),
            focused_search_once: false,
            deleting_profile: false,
            opening_rdp_profile: None,
            profile_form_subscription: None,
            jumpserver_subscription: None,
            notice: None,
            view_mode: ViewMode::Manager,
            workspaces: Vec::new(),
            path_favorites: HashMap::new(),
            active_workspace_id: None,
            next_terminal_id: 1,
            load_generation: 0,
            capability_generation: 0,
            persist_generation: 0,
            last_transfer_revision: 0,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load_initial_state(window, cx);
        this.spawn_refresh_ticker(window, cx);
        this
    }

    pub(super) fn profile_connection_available(&self, profile: &SshProfile) -> bool {
        profile.ssh_path.is_some() || matches!(self.default_capability.as_ref(), Some(Ok(_)))
    }

    fn spawn_refresh_ticker(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, async_cx| {
            loop {
                async_cx
                    .background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if this
                    .update_in(async_cx, |this, _window, cx| {
                        let revision = this.service.transfer_revision();
                        if revision != this.last_transfer_revision {
                            this.last_transfer_revision = revision;
                            cx.notify();
                        } else if this.has_live_terminals(cx) {
                            // 终端退出状态属于子视图；低频刷新标签状态即可。
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Focusable for SshView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
