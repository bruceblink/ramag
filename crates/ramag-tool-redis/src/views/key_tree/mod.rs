//! Key 树：增量 SCAN 分批加载（服务端 MATCH 下推 + 进度 + 可停止，状态机在 scan.rs），
//! 按 `:` 折叠命名空间。同时是叶子+命名空间的节点（`user` 与 `user:1` 共存）单击仅展开，
//! 类型 badge 才加载值

mod ops;
mod render;
mod scan;
mod tree;

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AppContext as _, ClickEvent, Context, Entity, EventEmitter, IntoElement, ParentElement, Render,
    Styled, UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{ConnectionConfig, KeyMeta};
use ramag_ui::platform::primary_shortcut;

use tree::{TreeNode, VisibleRow, build_tree, collect_namespace_paths, has_match_descendant};

/// 单次最多加载的 key 数（防爆内存）
const MAX_KEYS: usize = 5_000;

/// 命名空间分隔符（业界事实标准）
const NAMESPACE_SEP: char = ':';

/// 单层缩进（像素）
pub(super) const INDENT_PX: f32 = 14.0;

#[derive(Debug, Clone)]
pub enum KeyTreeEvent {
    /// 用户选中某个 key
    Selected(String),
    /// 请求新建 Key（点击顶部 "+" 按钮）；由上层弹出 KeyCreateForm 对话框处理
    RequestCreate,
    /// 请求打开命令行控制台（点击工具栏命令行图标）；由 Session 展开右侧浮层
    RequestOpenConsole,
    /// 用户切换 DB（0-15）；由 Session 处理（同步详情 + 重新加载树）
    DbSelected(u8),
    /// 树侧右键删除完成（key / 前缀 / 整库）；Session 据此清理详情面板
    KeysDeleted(DeletedScope),
}

/// 树侧删除操作的影响范围
#[derive(Debug, Clone)]
pub enum DeletedScope {
    /// 单个 key
    Key(String),
    /// 前缀路径（如 "user"，实际删除 user:* 全部）
    Prefix(String),
    /// 当前 DB 整库（FLUSHDB）
    Db,
}

pub struct KeyTreePanel {
    service: Arc<RedisService>,
    config: Option<ConnectionConfig>,
    db: u8,
    /// 已加载（缓存）的 key 列表（原始顺序）
    keys: Vec<KeyMeta>,
    /// 已加载 key 名集合：SCAN 弱一致会跨批重复返回同一 key，追加前据此去重
    /// （否则计数虚高、Trie 重复插入）
    seen_keys: HashSet<String>,
    /// 已加载 key 的 Trie 树（按 NAMESPACE_SEP 分层）
    tree: Vec<TreeNode>,
    /// 已展开的命名空间路径集合（按 full_path 索引）
    expanded: HashSet<String>,
    loading: bool,
    error: Option<String>,
    /// 客户端搜索框 / 关键字（小写）
    search: Entity<InputState>,
    query: String,
    /// 服务端 MATCH 模式（Enter 下推触发重扫）；None = 全库扫描
    match_pattern: Option<String>,
    /// 扫描代际：换代（停止 / 重扫 / 换 pattern / 切库）后在途批次回包一律作废
    scan_generation: u64,
    /// 上次重建 Trie 时的 key 数（分批加载期间节流重建，避免每批 O(N) 重建）
    last_rebuilt_count: usize,
    /// 当前选中的 key（高亮）
    selected: Option<String>,
    /// 是否到达 MAX_KEYS 截断
    truncated: bool,
    /// 虚拟列表滚动句柄：树扁平化后用 uniform_list 行级虚拟化，
    /// 支持 5w+ key 仍流畅
    uniform_scroll: UniformListScrollHandle,
    /// 右键删除操作完成后的 toast，下次 render 推送
    pending_notification: Option<gpui_component::notification::Notification>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<KeyTreeEvent> for KeyTreePanel {}

impl KeyTreePanel {
    pub fn new(service: Arc<RedisService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("过滤 key"));

        let subs = vec![cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, e: &InputEvent, _, cx| match e {
                InputEvent::Change => {
                    this.query = this.search.read(cx).value().trim().to_lowercase();
                    // 清空搜索框时撤掉服务端 MATCH 重扫全库，避免状态栏计数与空搜索框互相矛盾
                    if this.query.is_empty() && this.match_pattern.is_some() {
                        this.match_pattern = None;
                        this.refresh(cx);
                    }
                    cx.notify();
                }
                // Enter：把关键字下推为服务端 MATCH 重扫（大库不再只能对已加载子集过滤）
                InputEvent::PressEnter { .. } => this.apply_server_match(cx),
                _ => {}
            },
        )];

        Self {
            service,
            config: None,
            db: 0,
            keys: Vec::new(),
            seen_keys: HashSet::new(),
            tree: Vec::new(),
            expanded: HashSet::new(),
            loading: false,
            error: None,
            search,
            query: String::new(),
            match_pattern: None,
            scan_generation: 0,
            last_rebuilt_count: 0,
            selected: None,
            truncated: false,
            uniform_scroll: UniformListScrollHandle::new(),
            pending_notification: None,
            _subscriptions: subs,
        }
    }

    /// 切换连接 / DB → 重新拉一次 SCAN
    pub fn set_connection(
        &mut self,
        config: Option<ConnectionConfig>,
        db: u8,
        cx: &mut Context<Self>,
    ) {
        self.config = config;
        self.db = db;
        self.selected = None;
        self.error = None;
        self.keys.clear();
        self.seen_keys.clear();
        self.tree.clear();
        self.expanded.clear();
        self.truncated = false;
        // 切连接/db：旧 SCAN 回包已由 refresh 内的 stale 校验拦截，这里清 loading
        // 让新目标的 refresh 不被防重入拒绝
        self.loading = false;
        if self.config.is_some() {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    /// 连接健康快照 (loading, has_error)：供上层 Tab 圆点显示真实连接状态
    pub fn health(&self) -> (bool, bool) {
        (self.loading, self.error.is_some())
    }

    /// 会话 Tab 被（重新）激活时调用：仅当从未成功加载（无 key 且非加载中）才 SCAN，
    /// 避免每次切 Tab 都重置展开/选中。首次加载失败留下的空状态会在下次激活时自动重试
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if self.config.is_some() && self.keys.is_empty() && !self.loading {
            self.refresh(cx);
        }
    }

    /// 由 keys 重建 Trie 树；默认展开第一层命名空间。记录重建时 key 数供分批加载节流
    fn rebuild_tree(&mut self) {
        self.tree = build_tree(&self.keys);
        self.last_rebuilt_count = self.keys.len();
        if self.expanded.is_empty() {
            for n in &self.tree {
                if n.is_namespace() {
                    self.expanded.insert(n.full_path.clone());
                }
            }
        }
    }

    fn select_key(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        cx.emit(KeyTreeEvent::Selected(key));
        cx.notify();
    }

    /// 外部触发选中（如新建 Key 后由 Session 调用）
    pub fn select_key_external(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        cx.emit(KeyTreeEvent::Selected(key));
        cx.notify();
    }

    fn toggle_expanded(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        cx.notify();
    }

    fn matches_query(&self, key: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        key.to_lowercase().contains(&self.query)
    }

    /// 把树扁平化为可见行列表（owned 结构，避免与 cx.listener 借用冲突）
    fn flatten_visible(&self) -> Vec<VisibleRow> {
        let mut out = Vec::new();
        let in_search = !self.query.is_empty();
        for n in &self.tree {
            self.collect_visible(n, 0, in_search, &mut out);
        }
        out
    }

    fn collect_visible(
        &self,
        node: &TreeNode,
        depth: usize,
        in_search: bool,
        out: &mut Vec<VisibleRow>,
    ) {
        let leaf_match = node.is_key && self.matches_query(&node.full_path);
        let descendant_match = node.is_namespace() && has_match_descendant(node, &self.query);

        if in_search && !leaf_match && !descendant_match {
            return;
        }

        let is_namespace = node.is_namespace();
        let is_expanded = if in_search {
            descendant_match
        } else {
            self.expanded.contains(&node.full_path)
        };

        out.push(VisibleRow {
            depth,
            label: node.label.clone(),
            full_path: node.full_path.clone(),
            is_key: node.is_key,
            leaf_type: node.leaf_type,
            is_namespace,
            is_expanded,
        });

        if is_namespace && is_expanded {
            for c in &node.children {
                self.collect_visible(c, depth + 1, in_search, out);
            }
        }
    }

    fn expand_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        for n in &self.tree {
            collect_namespace_paths(n, &mut self.expanded);
        }
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        cx.notify();
    }
}

impl Render for KeyTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 右键删除操作异步完成的 toast 在这里推送
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
        let visible = self.flatten_visible();
        let visible_leaf_count = visible.iter().filter(|r| r.is_key).count();
        let selected = self.selected.clone();

        // 状态栏：扫描中报进度；带服务端 MATCH 时标注模式，区别于「共 N（全库）」
        let pattern_note = self
            .match_pattern
            .as_deref()
            .map(|p| format!("MATCH {p} · "))
            .unwrap_or_default();
        let count_label = if self.config.is_none() {
            "尚未连接".to_string()
        } else if self.loading {
            format!("{pattern_note}已加载 {total} 个 key…（扫描中，可点⏹停止）")
        } else if let Some(ref e) = self.error {
            e.clone()
        } else if !in_search {
            format!(
                "{pattern_note}共 {total} 个 key{}",
                if self.truncated {
                    "（已达上限截断）"
                } else {
                    ""
                }
            )
        } else {
            format!("{pattern_note}匹配 {visible_leaf_count} / {total}")
        };

        // 顶部第 1 行：DB 选择
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
                Button::new("kt-db-picker")
                    .ghost()
                    .small()
                    .label(db_picker_label)
                    .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
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
                            m = m.item(PopupMenuItem::new(label).on_click(move |_, _, app| {
                                entity.update(app, |this, cx| {
                                    if this.db != db {
                                        cx.emit(KeyTreeEvent::DbSelected(db));
                                    }
                                });
                            }));
                        }
                        // 自建实例可配 databases > 16：提供自由输入入口（0-255）
                        let entity_for_prompt = session_entity.clone();
                        m = m.item(PopupMenuItem::new("  其他 DB…").on_click(
                            move |_, window, app| {
                                let entity = entity_for_prompt.clone();
                                ramag_ui::open_prompt(
                                    "切换 DB",
                                    "输入 DB 序号（0-255，须不超过服务端 databases 配置）"
                                        .to_string(),
                                    "",
                                    "切换",
                                    move |value, _window, app| match value.trim().parse::<u8>() {
                                        Ok(db) => {
                                            entity.update(app, |this, cx| {
                                                if this.db != db {
                                                    cx.emit(KeyTreeEvent::DbSelected(db));
                                                }
                                            });
                                        }
                                        Err(_) => {
                                            tracing::warn!("invalid redis db input ignored");
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

        // 顶部第 2 行：搜索 + 新建 Key + 全展开 / 全折叠 / 刷新
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
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small().text_color(muted_fg)),
                ),
            )
            .child(
                Button::new("redis-key-create")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        cx.emit(KeyTreeEvent::RequestCreate);
                    })),
            )
            .child({
                let any_expanded = !self.expanded.is_empty();
                let (icon, tip) = if any_expanded {
                    (IconName::FolderOpen, "全部折叠命名空间")
                } else {
                    (IconName::FolderClosed, "全部展开命名空间")
                };
                Button::new("redis-key-toggle-all")
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
            .child({
                // 扫描中该位变「停止」：保留已加载部分，随时可中断大库扫描
                let scanning = self.loading;
                let (icon, tip) = if scanning {
                    (Icon::new(IconName::CircleX), "停止扫描（保留已加载）")
                } else {
                    (ramag_ui::icons::refresh_cw(), "重新扫描")
                };
                Button::new("redis-key-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if scanning {
                            this.stop_scan(cx);
                        } else {
                            this.refresh(cx);
                        }
                    }))
            })
            .child(
                Button::new("redis-open-console")
                    .ghost()
                    .xsmall()
                    .icon(IconName::SquareTerminal)
                    .tooltip(format!("命令行（{}）", primary_shortcut("E")))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        cx.emit(KeyTreeEvent::RequestOpenConsole);
                    })),
            )
            .child({
                // DB 级毁灭性操作独立入口（清空当前 DB），不与 key 右键菜单混排
                let entity_for_menu = cx.entity().clone();
                let current_db = self.db;
                Button::new("redis-key-more")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::ellipsis())
                    .tooltip("更多操作")
                    .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| {
                        ops::toolbar_more_menu(menu, entity_for_menu.clone(), current_db)
                    })
            });

        let theme_bg = theme.background;
        let theme_muted = theme.muted;

        // 树形渲染：扁平化为 Vec<VisibleRow>，喂给 uniform_list 行级虚拟化
        let visible_rc: Rc<Vec<VisibleRow>> = Rc::new(visible);
        let row_count = visible_rc.len();

        let empty_hint =
            !self.loading && total == 0 && self.config.is_some() && self.error.is_none();

        let body: gpui::AnyElement = if row_count == 0 {
            if empty_hint {
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

        let status_bar = div()
            .flex_none()
            .w_full()
            .px(px(10.0))
            .py(px(4.0))
            .border_t_1()
            .border_color(border)
            .text_xs()
            .text_color(muted_fg)
            .child(count_label);

        v_flex()
            .size_full()
            .bg(bg)
            .child(db_row)
            .child(header)
            .child(body)
            .child(status_bar)
    }
}
