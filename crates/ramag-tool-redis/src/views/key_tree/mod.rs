//! 基于增量 SCAN 与命名空间折叠的 Redis Key 树。

mod ops;
mod render;
mod scan;
mod transfer_ops;
mod tree;

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

/// 当前 DB 的 key 树加载上限（各 DB 独立）。列表、去重集合与 Trie 三份索引
/// 共同受此计数和原始名称字节预算约束：100 万 key（名称均长 ~30B）约占
/// 300-500 MB，是桌面场景合理极限；再大提示用 MATCH 缩小范围
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
    /// 前缀路径（如 "user"，实际删除 user:* 全部）
    Prefix(String),
    Db,
}

pub struct KeyTreePanel {
    service: Arc<RedisService>,
    config: Option<ConnectionConfig>,
    db: u8,
    keys: Vec<KeyMeta>,
    /// 已加载 key 名集合：SCAN 弱一致会跨批重复返回同一 key，追加前据此去重
    /// （否则计数虚高、Trie 重复插入）
    seen_keys: HashSet<String>,
    /// `keys` 中原始 Key 名的总字节数；避免超长名称在多份树索引中放大内存。
    key_bytes: usize,
    tree: Vec<TreeNode>,
    expanded: HashSet<String>,
    /// Trie 内容与展开状态代次；可见行缓存只依赖这两者和查询词。
    tree_revision: u64,
    expanded_revision: u64,
    visible_rows_cache: RefCell<Option<VisibleRowsCacheEntry>>,
    loading: bool,
    /// 至少收到过一批有效 SCAN 回包；空数据库也算已加载，避免 Tab 激活时反复重扫。
    has_loaded: bool,
    error: Option<String>,
    search: Entity<InputState>,
    query: String,
    /// 服务端 MATCH 模式（Enter 下推触发重扫）；None = 全库扫描
    match_pattern: Option<String>,
    /// 搜索输入防抖代际与等待态；停顿后自动下推 MATCH，Enter 可立即触发。
    search_generation: u64,
    search_pending: bool,
    /// 扫描代际：换代（停止 / 重扫 / 换 pattern / 切库）后在途批次回包一律作废
    scan_generation: u64,
    /// 上次重建 Trie 时的 key 数（分批加载期间节流重建，避免每批 O(N) 重建）
    last_rebuilt_count: usize,
    selected: Option<String>,
    /// 手动停止或批次出错后暂停，仍可从断点继续扫描。
    truncated: bool,
    /// 达到全局资源上限后不再提供继续扫描，需用 MATCH 缩小范围。
    resource_limited: bool,
    /// 下一次应继续使用的 SCAN cursor；None 表示已经完整扫完。
    resume_cursor: Option<u64>,
    /// 虚拟列表滚动句柄：树扁平化后用 uniform_list 行级虚拟化，
    /// 支持 5w+ key 仍流畅
    uniform_scroll: UniformListScrollHandle,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pending_notification: Option<gpui_component::notification::Notification>,
    /// 树级写操作串行化闸门；切换连接或 DB 后旧任务 token 失效。
    mutation_gate: AsyncMutationGate,
    transfer: ramag_ui::TransferState,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<KeyTreeEvent> for KeyTreePanel {}

impl KeyTreePanel {
    pub fn new(service: Arc<RedisService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("全库搜索 key（支持 * ? [）")
        });

        let subs = vec![cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, e: &InputEvent, _, cx| match e {
                InputEvent::Change => {
                    this.query = this.search.read(cx).value().trim().to_lowercase();
                    this.schedule_server_match(cx);
                }
                // Enter 跳过去抖立即全库搜索。
                InputEvent::PressEnter { .. } => {
                    this.search_generation = this.search_generation.wrapping_add(1);
                    this.search_pending = false;
                    this.apply_server_match(cx);
                }
                _ => {}
            },
        )];

        Self {
            service,
            config: None,
            db: 0,
            keys: Vec::new(),
            seen_keys: HashSet::new(),
            key_bytes: 0,
            tree: Vec::new(),
            expanded: HashSet::new(),
            tree_revision: 0,
            expanded_revision: 0,
            visible_rows_cache: RefCell::new(None),
            loading: false,
            has_loaded: false,
            error: None,
            search,
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
        self.expanded_revision = self.expanded_revision.wrapping_add(1);
        self.truncated = false;
        self.resource_limited = false;
        self.resume_cursor = None;
        self.search_pending = false;
        // 切连接/db：旧 SCAN 回包已由 refresh 内的 stale 校验拦截，这里清 loading
        // 让新目标的 refresh 不被防重入拒绝
        self.loading = false;
        self.has_loaded = false;
        if self.config.is_some() {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    /// key 元数据加载快照 (loading, has_error)，不代表实时连接健康。
    pub fn health(&self) -> (bool, bool) {
        (self.loading, self.error.is_some())
    }

    /// 会话 Tab 被（重新）激活时调用：仅当从未成功加载（无 key 且非加载中）才 SCAN，
    /// 避免每次切 Tab 都重置展开/选中。首次加载失败留下的空状态会在下次激活时自动重试
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if should_ensure_loaded(self.config.is_some(), self.has_loaded, self.loading) {
            self.refresh(cx);
        }
    }

    fn rebuild_tree(&mut self) {
        self.tree = build_tree(&self.keys);
        self.last_rebuilt_count = self.keys.len();
        let expanded_changed = prune_expanded_for_tree(&self.tree, &mut self.expanded);
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

    fn toggle_expanded(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.expanded_revision = self.expanded_revision.wrapping_add(1);
        self.visible_rows_cache.get_mut().take();
        cx.notify();
    }

    pub(super) fn is_read_only(&self) -> bool {
        self.config.as_ref().is_some_and(|config| config.production)
    }

    pub(super) fn operation_context_matches(&self, config: &ConnectionConfig, db: u8) -> bool {
        self.db == db && self.config.as_ref().map(|current| &current.id) == Some(&config.id)
    }
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

impl Render for KeyTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            use gpui_component::WindowExt as _;
            window.push_notification(n, cx);
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let bg = theme.background;
        let row_hover = theme.muted;
        let accent = theme.accent;

        let total = self.keys.len();
        let in_search = !self.query.is_empty();
        let (visible_rc, visible_leaf_count) = self.visible_rows();
        let selected = self.selected.clone();
        let read_only = self.is_read_only();
        let mutating = self.mutation_gate.is_busy();

        // 状态栏：扫描中报进度；带服务端 MATCH 时标注模式，区别于「共 N（全库）」
        let pattern_note = self
            .match_pattern
            .as_deref()
            .map(|p| format!("MATCH {p} · "))
            .unwrap_or_default();
        let mut count_label = if self.config.is_none() {
            "尚未连接".to_string()
        } else if self.search_pending {
            format!("正在准备全库搜索“{}”…", self.query)
        } else if self.loading {
            format!("{pattern_note}已加载 {total} 个 key…（扫描中，可点⏹停止）")
        } else if let Some(ref e) = self.error {
            e.clone()
        } else if !in_search {
            format!(
                "{pattern_note}共 {total} 个 key{}",
                if self.truncated {
                    "（已停止，可继续扫描）"
                } else {
                    ""
                }
            )
        } else {
            format!("{pattern_note}匹配 {visible_leaf_count} / {total}")
        };
        if self.resource_limited {
            count_label.push_str(" · 已达到安全上限，请用 MATCH 缩小范围");
        } else if self.key_bytes >= INTERACTIVE_RESULT_WARNING_BYTES {
            count_label.push_str(" · 名称缓存已超过 128 MiB，建议用 MATCH 缩小范围");
        }
        if mutating {
            count_label.push_str(" · 写操作执行中…");
        }

        let current_db = self.db;
        let session_entity = cx.entity();
        let db_picker_label = format!("DB {current_db} ▾");
        let db_row = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .gap(px(8.0))
            .items_center()
            .child(
                ramag_ui::clickable_button("kt-db-picker")
                    .ghost()
                    .small()
                    .label(db_picker_label)
                    .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                        let mut m = menu;
                        let entity = session_entity.clone();
                        // 常规列 0-15；当前 db 更高（自建实例 databases > 16）时并入列表可回切
                        let mut dbs: Vec<u8> = (0u8..=15).collect();
                        if current_db > 15 {
                            dbs.push(current_db);
                        }
                        for db in dbs {
                            let is_active = db == current_db;
                            let label = if is_active {
                                format!("✓ DB {db}")
                            } else {
                                format!("  DB {db}")
                            };
                            let entity = entity.clone();
                            m = m.item(ramag_ui::menu_item(label).on_click(move |_, _, app| {
                                entity.update(app, |this, cx| {
                                    if this.db != db {
                                        cx.emit(KeyTreeEvent::DbSelected(db));
                                    }
                                });
                            }));
                        }
                        // 自建实例可配 databases > 16：提供自由输入入口（0-255）
                        let entity_for_prompt = session_entity.clone();
                        m = m.item(ramag_ui::menu_item("  自定义").on_click(
                            move |_, window, app| {
                                let entity = entity_for_prompt.clone();
                                ramag_ui::open_bounded_prompt(
                                    "切换 DB",
                                    "输入 DB 序号（0-255，须不超过服务端 databases 配置）"
                                        .to_string(),
                                    "",
                                    "切换",
                                    3,
                                    move |value, _window, app| match value.trim().parse::<u8>() {
                                        Ok(db) => {
                                            entity.update(app, |this, cx| {
                                                if this.db != db {
                                                    cx.emit(KeyTreeEvent::DbSelected(db));
                                                }
                                            });
                                        }
                                        Err(_) => {
                                            entity.update(app, |this, cx| {
                                                this.pending_notification = Some(
                                                    gpui_component::notification::Notification::error(
                                                        "DB 序号无效，请输入 0-255 的整数",
                                                    ),
                                                );
                                                cx.notify();
                                            });
                                        }
                                    },
                                    window,
                                    app,
                                );
                            },
                        ));
                        m
                    }),
            );

        let header = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .gap(px(6.0))
            .items_center()
            .child(
                div().flex_1().min_w_0().child(
                    ramag_ui::cleanable_input(&self.search, "redis-key-search-clear", false, cx)
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted_fg)),
                ),
            )
            .child({
                let scanning = self.loading;
                let icon = if scanning {
                    Icon::new(IconName::CircleX)
                } else {
                    ramag_ui::icons::refresh_cw()
                };
                ramag_ui::clickable_button("redis-key-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if scanning {
                            this.stop_scan(cx);
                        } else {
                            this.refresh(cx);
                        }
                    }))
            })
            .child({
                let any_expanded = !self.expanded.is_empty();
                let (icon, tip) = if any_expanded {
                    (IconName::FolderOpen, "折叠")
                } else {
                    (IconName::FolderClosed, "展开")
                };
                ramag_ui::clickable_button("redis-key-toggle-all")
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if any_expanded {
                            this.collapse_all(cx);
                        } else {
                            this.expand_all(cx);
                        }
                    }))
            })
            .child(
                ramag_ui::clickable_button("redis-open-console")
                    .ghost()
                    .xsmall()
                    .icon(IconName::SquareTerminal)
                    .tooltip("命令行")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        cx.emit(KeyTreeEvent::RequestOpenConsole);
                    })),
            )
            .child({
                let entity_for_menu = cx.entity().clone();
                let current_db = self.db;
                let more_tip: Option<&'static str> = if read_only {
                    Some("只读")
                } else if mutating {
                    Some("操作进行中")
                } else {
                    None
                };
                ramag_ui::clickable_button("redis-key-more")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::ellipsis())
                    .disabled(read_only || mutating)
                    .when_some(more_tip, |b, tip| b.tooltip(tip))
                    // 菜单顶部左角锚在按钮上，向右下方展开（不往上弹遮挡工具栏）
                    .pointer_dropdown_menu_with_anchor(gpui::Anchor::TopLeft, move |menu, _, _| {
                        ops::toolbar_more_menu(menu, entity_for_menu.clone(), current_db)
                    })
            });

        let theme_bg = theme.background;
        let theme_muted = theme.muted;

        let row_count = visible_rc.len();

        let empty_hint =
            !self.loading && total == 0 && self.config.is_some() && self.error.is_none();

        let body: gpui::AnyElement = if row_count == 0 {
            if self.search_pending {
                div()
                    .flex_1()
                    .min_h_0()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(muted_fg)
                    .child("正在全库搜索…")
                    .into_any_element()
            } else if empty_hint {
                // 空态分场景：服务端 MATCH 零命中 ≠ 空库；本地过滤零命中另有「匹配 0/N」计数
                let hint = match &self.match_pattern {
                    Some(p) => format!("没有匹配 MATCH {p} 的 key（服务端已全库扫描）"),
                    None => "DB 内没有 key".to_string(),
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(muted_fg)
                    .child(hint)
                    .into_any_element()
            } else if !self.loading && in_search && self.error.is_none() {
                div()
                    .flex_1()
                    .min_h_0()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(muted_fg)
                    .child(format!("没有匹配“{}”的 key", self.query))
                    .into_any_element()
            } else {
                div().flex_1().min_h_0().into_any_element()
            }
        } else {
            let visible_for_closure = visible_rc.clone();
            let selected_for_closure = selected.clone();
            uniform_list(
                "redis-key-tree-rows",
                row_count,
                cx.processor(move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| {
                            let row_data = &visible_for_closure[i];
                            this.render_node_row(
                                i,
                                row_data,
                                &selected_for_closure,
                                fg,
                                muted_fg,
                                row_hover,
                                accent,
                                theme_bg,
                                theme_muted,
                                cx,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.uniform_scroll)
            .flex_1()
            .into_any_element()
        };

        let can_load_more = self.truncated
            && !self.resource_limited
            && self.resume_cursor.is_some()
            && !self.loading;
        let status_bar = h_flex()
            .flex_none()
            .w_full()
            .items_center()
            .justify_between()
            .px(px(10.0))
            .py(px(4.0))
            .border_t_1()
            .border_color(border)
            .text_xs()
            .text_color(muted_fg)
            .child(count_label)
            .when(can_load_more, |bar| {
                bar.child(
                    ramag_ui::clickable_button("redis-key-load-more")
                        .ghost()
                        .xsmall()
                        .label("继续")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.load_more(cx);
                        })),
                )
            });

        let transfer_row = ramag_ui::transfer_progress_row(
            "redis-transfer-cancel",
            &self.transfer,
            |this: &mut Self| &this.transfer,
            cx,
        );

        v_flex()
            .size_full()
            .bg(bg)
            .child(db_row)
            .child(header)
            .children(transfer_row)
            .child(body)
            .child(status_bar)
    }
}

#[cfg(test)]
mod load_state_tests {
    use super::{
        VisibleRowsCacheEntry, VisibleRowsCacheKey, build_tree, prune_expanded_for_tree,
        should_ensure_loaded,
    };
    use ramag_domain::entities::KeyMeta;
    use std::collections::HashSet;
    use std::rc::Rc;

    #[test]
    fn empty_but_successful_scan_is_not_reloaded_on_activation() {
        assert!(should_ensure_loaded(true, false, false));
        assert!(!should_ensure_loaded(true, true, false));
        assert!(!should_ensure_loaded(true, false, true));
        assert!(!should_ensure_loaded(false, false, false));
    }

    #[test]
    fn visible_rows_cache_is_scoped_to_tree_expansion_and_query() {
        let rows = Rc::new(Vec::new());
        let key = VisibleRowsCacheKey {
            tree_revision: 4,
            expanded_revision: 2,
            query: "user".into(),
        };
        let cache = VisibleRowsCacheEntry {
            key: key.clone(),
            rows: rows.clone(),
            leaf_count: 3,
        };

        let cached = cache.get(&key);
        assert!(cached.is_some());
        if let Some((cached_rows, leaf_count)) = cached {
            assert!(Rc::ptr_eq(&cached_rows, &rows));
            assert_eq!(leaf_count, 3);
        }

        let mut changed = key;
        changed.expanded_revision += 1;
        assert!(cache.get(&changed).is_none());
    }

    #[test]
    fn rebuilding_tree_drops_expansion_paths_from_old_searches() {
        let tree = build_tree(&[KeyMeta::bare("user:1")]);
        let mut expanded = HashSet::from(["user".to_string(), "old:path".to_string()]);

        assert!(prune_expanded_for_tree(&tree, &mut expanded));
        assert_eq!(expanded, HashSet::from(["user".to_string()]));
    }
}
