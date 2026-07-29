//! SSH 工具根视图与状态模型。

mod file_chunk;
mod file_preview;
mod file_syntax;
mod model;
mod ops;
mod ops_files;
mod ops_profile;
mod ops_transfer;
mod profile_dialog;
mod profile_form;
mod render;
mod render_directory_helpers;
mod render_manager;
mod render_profile_form;
mod render_transfers;
mod render_workspace;

#[cfg(test)]
mod render_test;

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
    workspace_resize: Entity<ResizableState>,
    focused_search_once: bool,
    deleting_profile: bool,
    profile_form_subscription: Option<Subscription>,
    notice: Option<Notice>,
    view_mode: ViewMode,
    workspaces: Vec<SshWorkspace>,
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
        let search =
            cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("搜索连接"));
        let directory_search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("搜索名称")
                .clean_on_escape()
        });
        let workspace_resize = cx.new(|_| ResizableState::default());
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
        subscriptions.push(ramag_ui::persist_resizable_sizes(
            &workspace_resize,
            "split_ssh_workspace",
            window,
            cx,
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
            workspace_resize,
            focused_search_once: false,
            deleting_profile: false,
            profile_form_subscription: None,
            notice: None,
            view_mode: ViewMode::Manager,
            workspaces: Vec::new(),
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
