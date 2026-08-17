//! 基于增量 SCAN 与命名空间折叠的 Redis Key 树。

mod guides;
mod helpers;
mod ops;
mod render;
#[cfg(test)]
mod render_test;
mod scan;
mod transfer_ops;
mod tree;
mod view;

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AppContext as _, ClickEvent, Context, Entity, EventEmitter, IntoElement, ParentElement, Render,
    Styled, UniformListScrollHandle, Window, div, prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{InputEvent, InputState},
    v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{
    ConnectionConfig, INTERACTIVE_RESULT_WARNING_BYTES, KeyMeta, MAX_INTERACTIVE_RESULT_BYTES,
    MAX_REDIS_LOADED_ITEMS,
};
use ramag_ui::AsyncMutationGate;
use ramag_ui::PointerDropdownMenu as _;

use tree::{TreeNode, VisibleRow, build_tree, collect_namespace_paths};

#[derive(Clone, PartialEq, Eq)]
struct VisibleRowsCacheKey {
    tree_revision: u64,
    expanded_revision: u64,
    query: String,
    sink_same_name_keys: bool,
}

struct VisibleRowsCacheEntry {
    key: VisibleRowsCacheKey,
    rows: Rc<Vec<VisibleRow>>,
    leaf_count: usize,
}

impl VisibleRowsCacheEntry {
    fn get(&self, key: &VisibleRowsCacheKey) -> Option<(Rc<Vec<VisibleRow>>, usize)> {
        (self.key == *key).then(|| (self.rows.clone(), self.leaf_count))
    }
}

/// 当前 DB 的加载上限，列表、去重和 Trie 共同受 key 数与名称字节预算约束。
/// 超限后提示使用 MATCH 缩小范围。
const MAX_LOADED_KEYS: usize = MAX_REDIS_LOADED_ITEMS;
const MAX_LOADED_KEY_BYTES: usize = MAX_INTERACTIVE_RESULT_BYTES;

const NAMESPACE_SEP: char = ':';

pub(super) const INDENT_PX: f32 = 14.0;

#[derive(Debug, Clone)]
pub enum KeyTreeEvent {
    Selected(String),
    RequestCreate,
    RequestOpenConsole,
    DbSelected(u8),
    KeysDeleted(DeletedScope),
}

#[derive(Debug, Clone)]
pub enum DeletedScope {
    Key(String),
    /// 前缀路径（如 "user"，删除 user:*）。
    Prefix(String),
    Db,
}

pub struct KeyTreePanel {
    service: Arc<RedisService>,
    config: Option<ConnectionConfig>,
    db: u8,
    keys: Vec<KeyMeta>,
    /// 已加载 key 名集合；SCAN 可能跨批重复返回，追加前据此去重。
    seen_keys: HashSet<String>,
    /// 原始 Key 名总字节数，避免树索引放大内存。
    key_bytes: usize,
    tree: Vec<TreeNode>,
    /// 普通浏览态的展开项；进入和退出搜索都不改变它。
    expanded: HashSet<String>,
    /// 搜索结果默认展开，仅记录当前搜索中主动折叠的命名空间。
    search_collapsed: HashSet<String>,
    /// Trie 与展开状态代际，供可见行缓存使用。
    tree_revision: u64,
    expanded_revision: u64,
    visible_rows_cache: RefCell<Option<VisibleRowsCacheEntry>>,
    loading: bool,
    /// 收到过有效 SCAN 回包；空库也算已加载，避免反复重扫。
    has_loaded: bool,
    error: Option<String>,
    search: Entity<InputState>,
    /// 输入框原文，用于忽略非文本变化通知。
    search_text: String,
    /// 本地不区分大小写匹配使用的小写查询词。
    query: String,
    /// 服务端 MATCH 模式；None 表示全库扫描。
    match_pattern: Option<String>,
    /// 搜索防抖代际与等待态；Enter 立即下推 MATCH。
    search_generation: u64,
    search_pending: bool,
    /// 扫描代际；停止、重扫、换模式或切库后使旧回包失效。
    scan_generation: u64,
    /// 上次重建 Trie 时的 key 数，用于批量加载节流。
    last_rebuilt_count: usize,
    selected: Option<String>,
    /// 手动停止或批次出错后暂停，可从断点继续扫描。
    truncated: bool,
    /// 达到资源上限后不再继续扫描，需用 MATCH 缩小范围。
    resource_limited: bool,
    /// 下次继续扫描的 cursor；None 表示已完整扫描。
    resume_cursor: Option<u64>,
    /// 虚拟列表滚动句柄。
    uniform_scroll: UniformListScrollHandle,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pending_notification: Option<gpui_component::notification::Notification>,
    /// 树级写操作闸门；切换连接或 DB 后旧任务失效。
    mutation_gate: AsyncMutationGate,
    transfer: ramag_ui::TransferState,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<KeyTreeEvent> for KeyTreePanel {}

impl KeyTreePanel {
    pub fn new(service: Arc<RedisService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("搜索 Key（支持 * ? [）")
        });

        let subs = vec![
            // set_value 不发 Change，需观察实体以响应清除和 Esc。
            cx.observe(&search, |this: &mut Self, _, cx| {
                this.sync_search_input(cx);
            }),
            cx.subscribe_in(
                &search,
                window,
                |this: &mut Self, _, e: &InputEvent, _, cx| {
                    if matches!(e, InputEvent::PressEnter { .. }) {
                        // Enter 跳过去抖，立即搜索。
                        this.search_generation = this.search_generation.wrapping_add(1);
                        this.search_pending = false;
                        this.apply_server_match(cx);
                    }
                },
            ),
        ];

        Self {
            service,
            config: None,
            db: 0,
            keys: Vec::new(),
            seen_keys: HashSet::new(),
            key_bytes: 0,
            tree: Vec::new(),
            expanded: HashSet::new(),
            search_collapsed: HashSet::new(),
            tree_revision: 0,
            expanded_revision: 0,
            visible_rows_cache: RefCell::new(None),
            loading: false,
            has_loaded: false,
            error: None,
            search,
            search_text: String::new(),
            query: String::new(),
            match_pattern: None,
            search_generation: 0,
            search_pending: false,
            scan_generation: 0,
            last_rebuilt_count: 0,
            selected: None,
            truncated: false,
            resource_limited: false,
            resume_cursor: None,
            uniform_scroll: UniformListScrollHandle::new(),
            pending_notification: None,
            mutation_gate: AsyncMutationGate::default(),
            transfer: ramag_ui::TransferState::default(),
            _subscriptions: subs,
        }
    }

    pub fn set_connection(
        &mut self,
        config: Option<ConnectionConfig>,
        db: u8,
        cx: &mut Context<Self>,
    ) {
        self.mutation_gate.reset();
        self.config = config;
        self.db = db;
        self.selected = None;
        self.error = None;
        self.keys.clear();
        self.seen_keys.clear();
        self.key_bytes = 0;
        self.clear_tree();
        self.expanded.clear();
        self.search_collapsed.clear();
        self.expanded_revision = self.expanded_revision.wrapping_add(1);
        self.truncated = false;
        self.resource_limited = false;
        self.resume_cursor = None;
        self.search_pending = false;
        // 旧回包由刷新时的 stale 校验拦截；清空 loading 以启动新刷新。
        self.loading = false;
        self.has_loaded = false;
        if self.config.is_some() {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    /// key 元数据加载快照，不代表实时连接健康。
    pub fn health(&self) -> (bool, bool) {
        (self.loading, self.error.is_some())
    }

    /// 激活 Tab 时仅在未成功加载且未加载中时 SCAN，避免重置展开和选中。
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if should_ensure_loaded(self.config.is_some(), self.has_loaded, self.loading) {
            self.refresh(cx);
        }
    }

    fn rebuild_tree(&mut self) {
        self.tree = build_tree(&self.keys);
        self.last_rebuilt_count = self.keys.len();
        // 增量扫描和 MATCH 搜索是局部快照，不能删除普通浏览态展开项。
        let has_complete_full_snapshot = self.match_pattern.is_none()
            && !self.loading
            && !self.truncated
            && !self.resource_limited
            && self.resume_cursor.is_none();
        let expanded_changed =
            has_complete_full_snapshot && prune_expanded_for_tree(&self.tree, &mut self.expanded);
        if expanded_changed {
            self.expanded_revision = self.expanded_revision.wrapping_add(1);
        }
        self.tree_revision = self.tree_revision.wrapping_add(1);
        self.visible_rows_cache.get_mut().take();
    }

    fn clear_tree(&mut self) {
        self.tree.clear();
        self.tree_revision = self.tree_revision.wrapping_add(1);
        self.visible_rows_cache.get_mut().take();
    }

    fn select_key(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        cx.emit(KeyTreeEvent::Selected(key));
        cx.notify();
    }

    pub fn select_key_external(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        cx.emit(KeyTreeEvent::Selected(key));
        cx.notify();
    }

    pub fn resolve_key_type(
        &mut self,
        key: &str,
        key_type: ramag_domain::entities::RedisType,
        cx: &mut Context<Self>,
    ) {
        if apply_resolved_key_type(&mut self.keys, key, key_type) {
            self.rebuild_tree();
            cx.notify();
        }
    }

    fn toggle_expanded(&mut self, path: String, cx: &mut Context<Self>) {
        let state = if self.query.is_empty() {
            &mut self.expanded
        } else {
            &mut self.search_collapsed
        };
        if !state.remove(&path) {
            state.insert(path);
        }
        self.expanded_revision = self.expanded_revision.wrapping_add(1);
        self.visible_rows_cache.get_mut().take();
        cx.notify();
    }

    fn sync_search_input(&mut self, cx: &mut Context<Self>) {
        let search_text = self.search.read(cx).value().trim().to_string();
        if search_text == self.search_text {
            return;
        }
        self.search_text = search_text;
        self.query = self.search_text.to_lowercase();
        if !self.search_collapsed.is_empty() {
            self.search_collapsed.clear();
            self.expanded_revision = self.expanded_revision.wrapping_add(1);
            self.visible_rows_cache.get_mut().take();
        }
        self.schedule_server_match(cx);
    }

    pub(super) fn is_read_only(&self) -> bool {
        self.config.as_ref().is_some_and(|config| config.production)
    }

    pub(super) fn operation_context_matches(&self, config: &ConnectionConfig, db: u8) -> bool {
        self.db == db && self.config.as_ref().map(|current| &current.id) == Some(&config.id)
    }
}

fn apply_resolved_key_type(
    keys: &mut [KeyMeta],
    key: &str,
    key_type: ramag_domain::entities::RedisType,
) -> bool {
    let Some(meta) = keys.iter_mut().find(|meta| meta.key == key) else {
        return false;
    };
    if meta.key_type == Some(key_type) {
        return false;
    }
    meta.key_type = Some(key_type);
    true
}

fn prune_expanded_for_tree(tree: &[TreeNode], expanded: &mut HashSet<String>) -> bool {
    let mut current = HashSet::new();
    for node in tree {
        collect_namespace_paths(node, &mut current);
    }
    let before = expanded.len();
    expanded.retain(|path| current.contains(path));
    expanded.len() != before
}

fn should_ensure_loaded(configured: bool, has_loaded: bool, loading: bool) -> bool {
    configured && !has_loaded && !loading
}

#[cfg(test)]
mod load_state_tests;
