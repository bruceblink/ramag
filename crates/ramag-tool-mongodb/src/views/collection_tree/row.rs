//! 树行扁平化 + 渲染（与 dbclient::table_tree::row 同款）。所有 TreeRow 变体高度统一 28px

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::{MongoDatabase, contains_case_insensitive};

use super::{CollectionTreePanel, ExpandedState, is_system_db};

#[derive(Clone)]
pub(super) enum TreeRow {
    Database {
        name: String,
        is_expanded: bool,
    },
    /// db 展开后的占位行：loading / error
    DbPlaceholder {
        text: String,
        is_error: bool,
    },
    Collection {
        db: String,
        name: String,
        is_view: bool,
    },
    /// 全局占位：加载 / 错误 / 空
    GlobalPlaceholder {
        text: String,
        is_error: bool,
    },
}

#[derive(Clone, PartialEq, Eq)]
struct TreeRowsCacheKey {
    tree_revision: u64,
    show_system: bool,
    filter: String,
}

#[derive(Clone)]
pub(super) struct TreeRowsView {
    pub(super) rows: Rc<Vec<TreeRow>>,
    pub(super) visible_databases: usize,
}

pub(super) struct TreeRowsCacheEntry {
    key: TreeRowsCacheKey,
    view: TreeRowsView,
}

impl TreeRowsCacheEntry {
    fn get(&self, key: &TreeRowsCacheKey) -> Option<TreeRowsView> {
        (self.key == *key).then(|| self.view.clone())
    }
}

impl CollectionTreePanel {
    /// 当前扁平树行；选中集合、编辑器显隐等普通重渲染直接复用。
    pub(super) fn tree_rows_view(&self, filter: &str) -> TreeRowsView {
        let key = TreeRowsCacheKey {
            tree_revision: self.tree_revision,
            show_system: self.show_system,
            filter: filter.to_string(),
        };
        {
            let cache = self.tree_rows_cache.borrow();
            if let Some(view) = cache.as_ref().and_then(|entry| entry.get(&key)) {
                return view;
            }
        }

        let view = build_tree_rows(
            &self.databases,
            &self.expanded,
            &self.open_databases,
            self.loading,
            self.error.as_deref(),
            self.show_system,
            filter,
        );
        self.tree_rows_cache.replace(Some(TreeRowsCacheEntry {
            key,
            view: view.clone(),
        }));
        view
    }

    /// 单行渲染（在 uniform_list 闭包内被调）；与 dbclient::table_tree::row 同款 28px 固定高度
    pub(super) fn render_tree_row(&self, row: &TreeRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let accent_fg = theme.accent_foreground;
        let muted_bg = theme.muted;
        let danger = theme.danger;

        match row {
            TreeRow::Database { name, is_expanded } => {
                let arrow = if *is_expanded { "▾" } else { "▸" };
                let name_for_click = name.clone();
                let name_for_menu = name.clone();
                let entity_for_menu = cx.entity().clone();
                h_flex()
                    .id(SharedString::from(format!("mongo-db-row-{name}")))
                    .h(px(28.0))
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |s| s.bg(muted_bg))
                    .child(
                        div()
                            .w(px(12.0))
                            .text_xs()
                            .text_color(muted_fg)
                            .child(SharedString::from(arrow.to_string())),
                    )
                    .child(Icon::new(IconName::HardDrive).small().text_color(muted_fg))
                    .child(
                        div()
                            .text_sm()
                            .text_color(fg)
                            .whitespace_nowrap()
                            .child(SharedString::from(name.clone())),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle_database(&name_for_click, cx)
                        }),
                    )
                    .context_menu(move |menu: PopupMenu, _, _| {
                        super::ops::database_context_menu(
                            menu,
                            entity_for_menu.clone(),
                            name_for_menu.clone(),
                        )
                    })
                    .into_any_element()
            }
            TreeRow::DbPlaceholder { text, is_error } => div()
                .h(px(28.0))
                .flex_none()
                .flex()
                .items_center()
                .px(px(28.0))
                .text_xs()
                .text_color(if *is_error { danger } else { muted_fg })
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            TreeRow::Collection { db, name, is_view } => {
                let selected =
                    self.selected
                        .as_ref()
                        .is_some_and(|(selected_db, selected_collection)| {
                            selected_db == db && selected_collection == name
                        });
                let db_for_click = db.clone();
                let name_for_click = name.clone();
                let db_for_menu = db.clone();
                let name_for_menu = name.clone();
                let is_view_for_menu = *is_view;
                let entity_for_menu = cx.entity().clone();
                let mut row = h_flex()
                    .id(SharedString::from(format!("mongo-coll-row-{db}-{name}")))
                    .h(px(28.0))
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .pl(px(40.0))
                    .pr_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |s| s.bg(muted_bg))
                    .child(
                        Icon::new(if *is_view {
                            IconName::Frame
                        } else {
                            IconName::MemoryStick
                        })
                        .small()
                        .text_color(muted_fg),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if selected { accent_fg } else { fg })
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(name.clone())),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_collection(db_for_click.clone(), name_for_click.clone(), cx)
                        }),
                    );
                if selected {
                    row = row.bg(accent);
                }
                row.context_menu(move |menu: PopupMenu, _, _| {
                    super::ops::collection_context_menu(
                        menu,
                        entity_for_menu.clone(),
                        db_for_menu.clone(),
                        name_for_menu.clone(),
                        is_view_for_menu,
                    )
                })
                .into_any_element()
            }
            TreeRow::GlobalPlaceholder { text, is_error } => div()
                .h(px(28.0))
                .flex_none()
                .flex()
                .items_center()
                .px(px(12.0))
                .text_xs()
                .text_color(if *is_error { danger } else { muted_fg })
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_tree_rows(
    databases: &[MongoDatabase],
    expanded: &HashMap<String, ExpandedState>,
    open_databases: &HashSet<String>,
    loading: bool,
    error: Option<&str>,
    show_system: bool,
    filter: &str,
) -> TreeRowsView {
    let mut rows = Vec::with_capacity(databases.len().saturating_mul(2));
    if loading && databases.is_empty() {
        rows.push(TreeRow::GlobalPlaceholder {
            text: "加载中…".into(),
            is_error: false,
        });
    }
    if let Some(error) = error {
        rows.push(TreeRow::GlobalPlaceholder {
            text: error.to_string(),
            is_error: true,
        });
    }
    if !loading && databases.is_empty() && error.is_none() {
        rows.push(TreeRow::GlobalPlaceholder {
            text: "（无数据库）".into(),
            is_error: false,
        });
    }

    let has_filter = !filter.is_empty();
    let mut visible_databases = 0;
    for database in databases {
        let name = &database.name;
        if !show_system && is_system_db(name) {
            continue;
        }
        let state = expanded.get(name);
        let database_matches = contains_case_insensitive(name, filter);
        let collection_matches = state.is_some_and(|state| {
            state
                .collections
                .iter()
                .any(|collection| contains_case_insensitive(&collection.name, filter))
        });
        if has_filter && !database_matches && !collection_matches {
            continue;
        }

        visible_databases += 1;
        let is_expanded = open_databases.contains(name) || (has_filter && state.is_some());
        rows.push(TreeRow::Database {
            name: name.clone(),
            is_expanded,
        });
        let Some(state) = state.filter(|_| is_expanded) else {
            continue;
        };

        if state.loading {
            rows.push(TreeRow::DbPlaceholder {
                text: "加载中…".into(),
                is_error: false,
            });
        }
        if let Some(error) = &state.error {
            rows.push(TreeRow::DbPlaceholder {
                text: error.clone(),
                is_error: true,
            });
        }
        if !state.loading && state.error.is_none() && state.collections.is_empty() {
            rows.push(TreeRow::DbPlaceholder {
                text: "（空）".into(),
                is_error: false,
            });
        }
        for collection in &state.collections {
            if has_filter
                && !database_matches
                && !contains_case_insensitive(&collection.name, filter)
            {
                continue;
            }
            rows.push(TreeRow::Collection {
                db: name.clone(),
                name: collection.name.clone(),
                is_view: collection.is_view,
            });
        }
    }

    TreeRowsView {
        rows: Rc::new(rows),
        visible_databases,
    }
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::MongoCollection;

    use super::*;

    #[test]
    fn search_loaded_cache_does_not_leave_database_open_after_filter_clears() {
        let databases = vec![MongoDatabase {
            name: "main".into(),
            size_on_disk: None,
            empty: false,
        }];
        let expanded = HashMap::from([(
            "main".into(),
            ExpandedState {
                collections: vec![MongoCollection {
                    name: "ÜBERblick".into(),
                    database: "main".into(),
                    is_view: false,
                }],
                ..Default::default()
            },
        )]);
        let open = HashSet::new();

        let searching = build_tree_rows(&databases, &expanded, &open, false, None, false, "über");
        assert!(searching.rows.iter().any(|row| {
            matches!(row, TreeRow::Collection { name, .. } if name == "ÜBERblick")
        }));

        let cleared = build_tree_rows(&databases, &expanded, &open, false, None, false, "");
        assert_eq!(cleared.rows.len(), 1);
        assert!(matches!(
            &cleared.rows[0],
            TreeRow::Database {
                is_expanded: false,
                ..
            }
        ));
    }
}
