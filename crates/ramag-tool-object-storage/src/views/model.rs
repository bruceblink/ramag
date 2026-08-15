use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, ScrollHandle, Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::resizable::ResizableState;
use ramag_app::ObjectStorageService;
use ramag_domain::entities::{
    ObjectCapabilities, ObjectEntry, ObjectListCursor, ObjectMetadata, ObjectStorageAccount,
    ObjectStorageAccountId, ObjectStorageFavorite, ObjectStorageMount, ObjectStorageMountId,
    ObjectStorageWorkspaceState, TransferCancellation,
};

#[derive(Clone, Copy)]
pub(super) enum AccountSessionState {
    Loading,
    Configured,
    Unverified,
}

pub(super) struct PendingTransferConflict {
    pub path: PathBuf,
    pub key: String,
    pub existing_summary: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObjectTransferDirection {
    Upload,
    Download,
}

impl ObjectTransferDirection {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Upload => "上传",
            Self::Download => "下载",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObjectTransferStatus {
    Completed,
    Failed,
    Cancelled,
}

pub(super) struct TransferHistoryUi {
    pub account_id: ObjectStorageAccountId,
    pub mount_id: ObjectStorageMountId,
    pub key: String,
    pub label: String,
    pub local_path: String,
    pub direction: ObjectTransferDirection,
    pub status: ObjectTransferStatus,
    pub error: Option<String>,
}

pub(super) struct TransferUi {
    pub id: u64,
    pub account_id: ObjectStorageAccountId,
    pub mount_id: ObjectStorageMountId,
    pub key: String,
    pub label: String,
    pub local_path: String,
    pub direction: ObjectTransferDirection,
    pub cancellation: TransferCancellation,
    pub transferred: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
}

pub struct ObjectStorageView {
    pub(super) service: Arc<ObjectStorageService>,
    pub(super) accounts: Arc<Vec<ObjectStorageAccount>>,
    pub(super) account_search: Entity<InputState>,
    pub(super) selected_account_id: Option<ObjectStorageAccountId>,
    pub(super) open_account_ids: Vec<ObjectStorageAccountId>,
    pub(super) session_preference_loaded: bool,
    pub(super) account_session_states: HashMap<ObjectStorageAccountId, AccountSessionState>,
    pub(super) management_visible: bool,
    pub(super) mounts: Arc<Vec<ObjectStorageMount>>,
    pub(super) mount_search: Entity<InputState>,
    pub(super) selected_mount: Option<ObjectStorageMount>,
    pub(super) capabilities: Option<ObjectCapabilities>,
    pub(super) show_mounts: bool,
    pub(super) show_detail: bool,
    pub(super) explorer_resize: Entity<ResizableState>,
    pub(super) object_filter: Entity<InputState>,
    pub(super) entries: Arc<Vec<ObjectEntry>>,
    pub(super) prefix: String,
    pub(super) next_cursor: Option<ObjectListCursor>,
    pub(super) listing_generation: Option<u64>,
    pub(super) listing_request_id: u64,
    pub(super) detail_request_id: u64,
    pub(super) selected_key: Option<String>,
    pub(super) detail_message: String,
    pub(super) detail_metadata: Option<ObjectMetadata>,
    pub(super) detail_scroll: ScrollHandle,
    pub(super) account_form_subscription: Option<Subscription>,
    pub(super) loading: bool,
    pub(super) notice: Option<(String, bool)>,
    pub(super) pending_upload: Option<PendingTransferConflict>,
    pub(super) pending_download: Option<PendingTransferConflict>,
    pub(super) upload_picker_open: bool,
    pub(super) download_picker_open: bool,
    pub(super) transfers: Vec<TransferUi>,
    pub(super) transfers_visible: bool,
    pub(super) next_transfer_id: u64,
    pub(super) transfer_history: VecDeque<TransferHistoryUi>,
    pub(super) workspace_states: Vec<ObjectStorageWorkspaceState>,
    pub(super) favorites: Vec<ObjectStorageFavorite>,
    pub(super) focus_handle: FocusHandle,
    pub(super) _subscriptions: Vec<Subscription>,
}

impl ObjectStorageView {
    pub fn new(
        service: Arc<ObjectStorageService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let account_search =
            cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("搜索账号名称…"));
        let mount_search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("筛选 Bucket / Region…")
        });
        let object_filter = cx
            .new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("筛选当前目录名称…"));
        let explorer_resize = cx.new(|_| ResizableState::default());
        let subscriptions = vec![
            cx.subscribe(
                &account_search,
                |_: &mut Self, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                },
            ),
            cx.subscribe(&mount_search, |_: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&object_filter, |_: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut this = Self {
            service,
            accounts: Arc::new(Vec::new()),
            account_search,
            selected_account_id: None,
            open_account_ids: Vec::new(),
            session_preference_loaded: false,
            account_session_states: HashMap::new(),
            management_visible: true,
            mounts: Arc::new(Vec::new()),
            mount_search,
            selected_mount: None,
            capabilities: None,
            show_mounts: false,
            show_detail: false,
            explorer_resize,
            object_filter,
            entries: Arc::new(Vec::new()),
            prefix: String::new(),
            next_cursor: None,
            listing_generation: None,
            listing_request_id: 0,
            detail_request_id: 0,
            selected_key: None,
            detail_message: "双击文件查看内容；右键可查看详情".into(),
            detail_metadata: None,
            detail_scroll: ScrollHandle::new(),
            account_form_subscription: None,
            loading: true,
            notice: None,
            pending_upload: None,
            pending_download: None,
            upload_picker_open: false,
            download_picker_open: false,
            transfers: Vec::new(),
            transfers_visible: false,
            next_transfer_id: 1,
            transfer_history: VecDeque::new(),
            workspace_states: Vec::new(),
            favorites: Vec::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load_accounts(window, cx);
        this.spawn_transfer_ticker(window, cx);
        this
    }

    fn spawn_transfer_ticker(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if this
                    .update_in(cx, |this, _window, cx| {
                        if !this.transfers.is_empty() {
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

impl Focusable for ObjectStorageView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
