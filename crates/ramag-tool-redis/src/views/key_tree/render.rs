//! 单行渲染 + 类型徽标。`impl KeyTreePanel`，闭包内调 select_key / toggle_expanded

use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::{
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::{RedisType, contains_case_insensitive};

use super::tree::{TreeNode, VisibleRow, collect_namespace_paths};
use super::{INDENT_PX, KeyTreePanel, VisibleRowsCacheEntry, VisibleRowsCacheKey};
use crate::views::inline_text_preview;

impl KeyTreePanel {
    /// 当前可见行与叶子数；选中态等普通重渲染直接复用，不重复克隆整棵树。
    pub(super) fn visible_rows(&self) -> (Rc<Vec<VisibleRow>>, usize) {
        let key = VisibleRowsCacheKey {
            tree_revision: self.tree_revision,
            expanded_revision: self.expanded_revision,
            query: self.query.clone(),
        };
        {
            let cache = self.visible_rows_cache.borrow();
            if let Some(cached) = cache.as_ref().and_then(|entry| entry.get(&key)) {
                return cached;
            }
        }

        let rows = Rc::new(flatten_visible_rows(
            &self.tree,
            &self.expanded,
            &self.search_collapsed,
            &self.query,
        ));
        let leaf_count = rows.iter().filter(|row| row.is_key).count();
        self.visible_rows_cache.replace(Some(VisibleRowsCacheEntry {
            key,
            rows: rows.clone(),
            leaf_count,
        }));
        (rows, leaf_count)
    }

    pub(super) fn expand_all(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            self.expanded.clear();
            for node in &self.tree {
                collect_namespace_paths(node, &mut self.expanded);
            }
        } else {
            self.search_collapsed.clear();
        }
        self.expanded_revision = self.expanded_revision.wrapping_add(1);
        self.visible_rows_cache.get_mut().take();
        cx.notify();
    }

    pub(super) fn collapse_all(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            self.expanded.clear();
        } else {
            self.search_collapsed.clear();
            for node in &self.tree {
                collect_namespace_paths(node, &mut self.search_collapsed);
            }
        }
        self.expanded_revision = self.expanded_revision.wrapping_add(1);
        self.visible_rows_cache.get_mut().take();
        cx.notify();
    }

    /// `+ use<>` 显式不捕获生命周期，避免返回值锁住 &self 与 cx.listener 借用冲突
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_node_row(
        &self,
        row_index: usize,
        row: &VisibleRow,
        selected: &Option<String>,
        fg: gpui::Hsla,
        muted_fg: gpui::Hsla,
        row_hover: gpui::Hsla,
        accent: gpui::Hsla,
        theme_bg: gpui::Hsla,
        theme_muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_namespace = row.is_namespace;
        // SCAN 装载的 key 不带类型（leaf_type=None），叶子判定必须用 is_key
        let is_leaf = row.is_key;
        let is_selected = is_leaf && selected.as_deref() == Some(row.full_path.as_str());

        let row_id = SharedString::from(format!("redis-tree-row-{row_index}"));
        let path_for_click = row.full_path.clone();
        let path_for_load = row.full_path.clone();

        // 折叠/展开图标（命名空间专属）。既是 key 又是命名空间时，展开只由 chevron 负责，
        // 行点击留给"加载值"（否则该 key 的值永远点不开）
        let chevron: gpui::AnyElement = if is_namespace {
            let glyph = if row.is_expanded { "▼" } else { "▶" };
            let path_for_chevron = row.full_path.clone();
            div()
                .id(SharedString::from(format!("redis-tree-chev-{row_index}")))
                .w(px(12.0))
                .text_xs()
                .text_color(muted_fg)
                .cursor_pointer()
                .child(glyph)
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_expanded(path_for_chevron.as_ref().clone(), cx);
                }))
                .into_any_element()
        } else {
            div().w(px(12.0)).into_any_element()
        };

        let type_badge: Option<gpui::AnyElement> = row.leaf_type.map(|t| {
            let path = path_for_load.clone();
            div()
                .id(SharedString::from(format!("redis-tree-badge-{row_index}")))
                .text_xs()
                .px(px(5.0))
                .py(px(1.0))
                .rounded(px(3.0))
                .bg(type_color_solid(t, theme_muted))
                .text_color(theme_bg)
                .cursor_pointer()
                .child(t.label())
                // badge 单击：始终加载值（不冒泡到行 toggle）
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.select_key(path.as_ref().clone(), cx);
                }))
                .into_any_element()
        });

        // 行点击：节点本身是 key 就加载值（既是 key 又是命名空间时，展开交给 chevron）；
        // 纯命名空间（非 key）才 toggle 展开。修复"user 与 user:1 共存时 user 点不开"
        let load_on_click = is_leaf;
        let on_row_click = cx.listener(move |this, _: &ClickEvent, _, cx| {
            if load_on_click {
                this.select_key(path_for_click.as_ref().clone(), cx);
            } else {
                this.toggle_expanded(path_for_click.as_ref().clone(), cx);
            }
        });

        let label_color = if is_namespace && !is_leaf {
            muted_fg
        } else {
            fg
        };

        // 显式行高 28px：uniform_list 行级虚拟化要求等高
        let mut row_el = h_flex()
            .id(row_id)
            .w_full()
            .h(px(28.0))
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .pl(px(8.0 + row.depth as f32 * INDENT_PX))
            .pr(px(10.0))
            .cursor_pointer()
            .child(chevron)
            .when_some(type_badge, |this, b| this.child(b))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(label_color)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(inline_text_preview(&row.label, 256)),
            )
            .on_click(on_row_click);

        if is_selected {
            let mut active_bg = accent;
            active_bg.a = 0.18;
            row_el = row_el.bg(active_bg);
        } else {
            row_el = row_el.hover(move |this| this.bg(row_hover));
        }

        // 生产连接仍允许只读导出，只从菜单中移除重命名和删除。
        let allow_write = !self.is_read_only();
        let entity_for_menu = cx.entity().clone();
        let path_for_menu = row.full_path.clone();
        row_el
            .context_menu(move |menu: PopupMenu, _, _| {
                super::ops::node_context_menu(
                    menu,
                    entity_for_menu.clone(),
                    path_for_menu.as_ref().clone(),
                    is_leaf,
                    is_namespace,
                    allow_write,
                )
            })
            .into_any_element()
    }
}

/// 把 Trie 扁平化为可见行。搜索模式走单次后序判定，避免每层重复扫描整棵子树。
fn flatten_visible_rows(
    tree: &[TreeNode],
    expanded: &std::collections::HashSet<String>,
    search_collapsed: &std::collections::HashSet<String>,
    query: &str,
) -> Vec<VisibleRow> {
    let mut rows = Vec::new();
    if query.is_empty() {
        for node in tree {
            collect_expanded_rows(node, 0, "", expanded, &mut rows);
        }
    } else {
        for node in tree {
            collect_search_rows(node, 0, "", query, search_collapsed, &mut rows);
        }
    }
    rows
}

fn collect_expanded_rows(
    node: &TreeNode,
    depth: usize,
    parent_path: &str,
    expanded: &std::collections::HashSet<String>,
    rows: &mut Vec<VisibleRow>,
) {
    let full_path = joined_path(parent_path, &node.label);
    let is_namespace = node.is_namespace();
    let is_expanded = is_namespace && expanded.contains(&full_path);
    rows.push(visible_row(node, depth, is_expanded, full_path.clone()));
    if is_expanded {
        for child in &node.children {
            collect_expanded_rows(child, depth + 1, &full_path, expanded, rows);
        }
    }
}

/// 返回当前节点子树是否含匹配 key；命中时已按父节点在前的顺序写入 rows。
fn collect_search_rows(
    node: &TreeNode,
    depth: usize,
    parent_path: &str,
    query: &str,
    search_collapsed: &std::collections::HashSet<String>,
    rows: &mut Vec<VisibleRow>,
) -> bool {
    let full_path = joined_path(parent_path, &node.label);
    let row_index = rows.len();
    rows.push(visible_row(node, depth, false, full_path.clone()));

    let self_matches = node.is_key && contains_case_insensitive(&full_path, query);
    let mut descendant_matches = false;
    for child in &node.children {
        descendant_matches |=
            collect_search_rows(child, depth + 1, &full_path, query, search_collapsed, rows);
    }

    if self_matches || descendant_matches {
        let is_expanded = descendant_matches && !search_collapsed.contains(&full_path);
        rows[row_index].is_expanded = is_expanded;
        if descendant_matches && !is_expanded {
            rows.truncate(row_index + 1);
        }
        true
    } else {
        rows.truncate(row_index);
        false
    }
}

fn visible_row(node: &TreeNode, depth: usize, is_expanded: bool, full_path: String) -> VisibleRow {
    VisibleRow {
        depth,
        label: if node.label.is_empty() && node.is_key {
            "（空 Key）".to_string()
        } else {
            node.label.clone()
        },
        full_path: Rc::new(full_path),
        is_key: node.is_key,
        leaf_type: node.leaf_type,
        is_namespace: node.is_namespace(),
        is_expanded,
    }
}

fn joined_path(parent: &str, label: &str) -> String {
    if parent.is_empty() {
        return label.to_string();
    }
    let mut path = String::with_capacity(parent.len().saturating_add(label.len() + 1));
    path.push_str(parent);
    path.push(super::NAMESPACE_SEP);
    path.push_str(label);
    path
}

/// 不同类型用不同色块（与 RedisInsight / zedis 配色靠拢）
/// 接受一个 fallback（None 类型 / theme.muted 等场景）避免依赖完整 theme 引用
fn type_color_solid(kind: RedisType, fallback: gpui::Hsla) -> gpui::Hsla {
    use gpui::hsla;
    match kind {
        RedisType::String => hsla(210.0 / 360.0, 0.6, 0.55, 1.0),
        RedisType::List => hsla(140.0 / 360.0, 0.5, 0.5, 1.0),
        RedisType::Hash => hsla(280.0 / 360.0, 0.55, 0.6, 1.0),
        RedisType::Set => hsla(40.0 / 360.0, 0.85, 0.55, 1.0),
        RedisType::ZSet => hsla(20.0 / 360.0, 0.7, 0.55, 1.0),
        RedisType::Stream => hsla(330.0 / 360.0, 0.55, 0.55, 1.0),
        RedisType::None => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::KeyMeta;

    #[test]
    fn search_flatten_visits_each_matching_branch_once() {
        let keys = vec![
            KeyMeta::bare("user:1:profile"),
            KeyMeta::bare("user:2:settings"),
            KeyMeta::bare("session:abc"),
        ];
        let tree = super::super::tree::build_tree(&keys);
        let rows = flatten_visible_rows(
            &tree,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            "profile",
        );
        let paths: Vec<&str> = rows.iter().map(|row| row.full_path.as_str()).collect();

        assert_eq!(paths, vec!["user", "user:1", "user:1:profile"]);
        assert!(rows[0].is_expanded);
        assert!(rows[1].is_expanded);
        assert!(!rows[2].is_expanded);
    }

    #[test]
    fn search_flatten_keeps_bare_key_without_type() {
        let tree = super::super::tree::build_tree(&[KeyMeta::bare("111")]);
        let rows = flatten_visible_rows(
            &tree,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            "111",
        );

        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_key);
        assert!(rows[0].leaf_type.is_none());
    }

    #[test]
    fn search_result_can_collapse_and_expand_without_changing_normal_state() {
        let tree = super::super::tree::build_tree(&[
            KeyMeta::bare("zset:1001"),
            KeyMeta::bare("zset:1002"),
        ]);
        let normal_expanded = std::collections::HashSet::from(["other".to_string()]);
        let collapsed = std::collections::HashSet::from(["zset".to_string()]);

        let collapsed_rows = flatten_visible_rows(&tree, &normal_expanded, &collapsed, "1002");
        assert_eq!(collapsed_rows.len(), 1);
        assert_eq!(collapsed_rows[0].full_path.as_str(), "zset");
        assert!(!collapsed_rows[0].is_expanded);

        let expanded_rows = flatten_visible_rows(
            &tree,
            &normal_expanded,
            &std::collections::HashSet::new(),
            "1002",
        );
        let paths: Vec<&str> = expanded_rows
            .iter()
            .map(|row| row.full_path.as_str())
            .collect();
        assert_eq!(paths, vec!["zset", "zset:1002"]);
        assert!(expanded_rows[0].is_expanded);
        assert_eq!(
            normal_expanded,
            std::collections::HashSet::from(["other".to_string()])
        );
    }
}
