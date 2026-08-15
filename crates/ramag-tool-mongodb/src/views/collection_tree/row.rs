//! MongoDB 对象树行，统一高度 28px。

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
        database_index: usize,
        is_expanded: bool,
    },
    DbPlaceholder {
        text: String,
        is_error: bool,
    },
    Collection {
        database_index: usize,
        collection_index: usize,
    },
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
    /// 普通重渲染复用已派生的树行。
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

    pub(super) fn render_tree_row(&self, row: &TreeRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let accent_fg = theme.accent_foreground;
        let muted_bg = theme.muted;
        let danger = theme.danger;

        match row {
            TreeRow::Database {
                database_index,
                is_expanded,
            } => {
                let Some(name) = self
                    .databases
                    .get(*database_index)
                    .map(|database| &database.name)
                else {
                    return div().h(px(28.0)).into_any_element();
                };
                let arrow = if *is_expanded { "▾" } else { "▸" };
                let name_for_click = name.clone();
                let name_for_menu = name.clone();
                let entity_for_menu = cx.entity().clone();
                h_flex()
                    .id(SharedString::from(format!("mongo-db-row-{database_index}")))
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
                    .on_click(
                        cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                            if event.modifiers().secondary() {
                                if ramag_ui::is_primary_modifier_double_click(event) {
                                    ramag_ui::copy_text_with_notification(
                                        name_for_click.clone(),
                                        window,
                                        cx,
                                    );
                                }
                                return;
                            }
                            this.toggle_database(&name_for_click, cx);
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
            TreeRow::Collection {
                database_index,
                collection_index,
            } => {
                let Some(db) = self
                    .databases
                    .get(*database_index)
                    .map(|database| &database.name)
                else {
                    return div().h(px(28.0)).into_any_element();
                };
                let Some(collection) = self
                    .expanded
                    .get(db)
                    .and_then(|state| state.collections.get(*collection_index))
                else {
                    return div().h(px(28.0)).into_any_element();
                };
                let name = &collection.name;
                let is_view = collection.is_view;
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
                let is_view_for_menu = is_view;
                let entity_for_menu = cx.entity().clone();
                let mut row = h_flex()
                    .id(SharedString::from(format!(
                        "mongo-coll-row-{database_index}-{collection_index}"
                    )))
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
                        Icon::new(if is_view {
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
                    .on_click(
                        cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                            if event.modifiers().secondary() {
                                if ramag_ui::is_primary_modifier_double_click(event) {
                                    ramag_ui::copy_text_with_notification(
                                        name_for_click.clone(),
                                        window,
                                        cx,
                                    );
                                }
                                return;
                            }
                            this.select_collection(
                                db_for_click.clone(),
                                name_for_click.clone(),
                                cx,
                            );
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
    for (database_index, database) in databases.iter().enumerate() {
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
            database_index,
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
        for (collection_index, collection) in state.collections.iter().enumerate() {
            if has_filter
                && !database_matches
                && !contains_case_insensitive(&collection.name, filter)
            {
                continue;
            }
            rows.push(TreeRow::Collection {
                database_index,
                collection_index,
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
        assert!(searching.rows.iter().any(|row| matches!(
            row,
            TreeRow::Collection {
                database_index: 0,
                collection_index: 0,
            }
        )));

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
