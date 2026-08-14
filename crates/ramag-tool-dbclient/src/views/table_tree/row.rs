//! 数据库对象树行，统一高度 28px。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::{Schema, contains_case_insensitive};

use super::{SchemaTables, TableColumns, TableTreePanel};
use crate::sql_completion::is_system_schema;
use crate::views::tree_helpers::{
    render_column_row, render_columns_placeholder, render_copyable_detail_line,
};

#[derive(Clone)]
pub(super) enum TreeRow {
    Schema {
        name: String,
        is_expanded: bool,
        is_system: bool,
    },
    SchemaPlaceholder {
        text: String,
        is_error: bool,
    },
    GroupHeader {
        text: String,
    },
    Table {
        key: Rc<(String, String)>,
        is_view: bool,
        is_cols_expanded: bool,
    },
    TablePlaceholder {
        text: String,
        is_error: bool,
    },
    Column {
        key: Rc<(String, String)>,
        column_index: usize,
    },
    SectionLabel {
        text: String,
    },
    DetailLine {
        element_id: SharedString,
        text: String,
        copy_value: String,
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
    pub(super) visible_schemas: usize,
    pub(super) searchable_schemas: usize,
    pub(super) failed_schemas: usize,
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

impl TableTreePanel {
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
            &self.schemas,
            &self.expanded,
            &self.open_schemas,
            &self.table_columns,
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
        let muted_fg = cx.theme().muted_foreground;
        let muted_bg = cx.theme().muted;
        let accent_bg = cx.theme().accent;
        let accent_fg = cx.theme().accent_foreground;
        let fg = cx.theme().foreground;
        let red = gpui::red();

        match row {
            TreeRow::Schema {
                name,
                is_expanded,
                is_system,
            } => {
                let arrow = if *is_expanded { "▾" } else { "▸" };
                let id_str = SharedString::from(format!("schema-{name}"));
                let name_for_click = name.clone();
                let name_for_copy = name.clone();
                let name_color = if *is_system { muted_fg } else { fg };
                let name_for_menu = name.clone();
                let entity_for_menu = cx.entity().clone();
                let driver = self.connection.as_ref().map(|c| c.driver);

                h_flex()
                    .id(id_str)
                    .h(px(28.0))
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |this| this.bg(muted_bg))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.modifiers().secondary() {
                            if ramag_ui::is_primary_modifier_double_click(event) {
                                ramag_ui::copy_text_with_notification(
                                    name_for_copy.clone(),
                                    window,
                                    cx,
                                );
                            }
                            return;
                        }
                        this.toggle_schema(name_for_click.clone(), cx);
                    }))
                    .child(
                        div()
                            .w(px(12.0))
                            .text_xs()
                            .text_color(muted_fg)
                            .child(arrow),
                    )
                    .child(Icon::new(IconName::HardDrive).small().text_color(muted_fg))
                    .child(
                        div()
                            .text_sm()
                            .text_color(name_color)
                            .whitespace_nowrap()
                            .child(name.clone()),
                    )
                    .context_menu(move |menu: PopupMenu, _, _| match driver {
                        Some(d) => super::ops::schema_context_menu(
                            menu,
                            entity_for_menu.clone(),
                            name_for_menu.clone(),
                            d,
                        ),
                        None => menu,
                    })
                    .into_any_element()
            }
            TreeRow::SchemaPlaceholder { text, is_error } => div()
                .w_full()
                .h(px(28.0))
                .flex_none()
                .pl_5()
                .pr_2()
                .pt(px(6.0))
                .text_xs()
                .text_color(if *is_error { red } else { muted_fg })
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(text.clone())
                .into_any_element(),
            TreeRow::GroupHeader { text } => div()
                .w_full()
                .h(px(28.0))
                .flex_none()
                .pl_5()
                .pr_2()
                .pt(px(6.0))
                .text_xs()
                .text_color(muted_fg)
                .child(text.clone())
                .into_any_element(),
            TreeRow::Table {
                key,
                is_view,
                is_cols_expanded,
            } => {
                let schema = &key.0;
                let name = &key.1;
                let is_selected =
                    self.selected
                        .as_ref()
                        .is_some_and(|(selected_schema, selected_table)| {
                            selected_schema == schema && selected_table == name
                        });
                let schema = schema.clone();
                let name = name.clone();
                let is_view = *is_view;
                let is_cols_expanded = *is_cols_expanded;

                let row_id = SharedString::from(format!("table-{}-{}", schema, name));
                let s_for_click = schema.clone();
                let t_for_click = name.clone();
                let t_for_copy = name.clone();

                let chevron_icon = if is_cols_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                let chevron_id = SharedString::from(format!("col-toggle-{}-{}", schema, name));
                let s_for_chev = schema.clone();
                let t_for_chev = name.clone();
                let s_for_menu = schema.clone();
                let t_for_menu = name.clone();
                let entity_for_menu = cx.entity().clone();

                let mut row = h_flex()
                    .id(row_id)
                    .h(px(28.0))
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .pl(px(20.0))
                    .pr_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |this| this.bg(muted_bg))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.modifiers().secondary() {
                            if ramag_ui::is_primary_modifier_double_click(event) {
                                ramag_ui::copy_text_with_notification(
                                    t_for_copy.clone(),
                                    window,
                                    cx,
                                );
                            }
                            return;
                        }
                        this.handle_table_click(s_for_click.clone(), t_for_click.clone(), cx);
                    }))
                    // 展开箭头不能触发表选择。
                    .child(
                        div()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .child(
                                ramag_ui::clickable_button(chevron_id)
                                    .ghost()
                                    .xsmall()
                                    .icon(chevron_icon)
                                    .tooltip(if is_cols_expanded {
                                        "收起字段"
                                    } else {
                                        "展开字段"
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.toggle_table_columns(
                                            s_for_chev.clone(),
                                            t_for_chev.clone(),
                                            cx,
                                        );
                                    })),
                            ),
                    )
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
                            .text_color(if is_selected { accent_fg } else { fg })
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(name.clone()),
                    );
                if is_selected {
                    row = row.bg(accent_bg);
                }
                let row = row.context_menu(move |menu: PopupMenu, _, _| {
                    super::ops::table_context_menu(
                        menu,
                        entity_for_menu.clone(),
                        s_for_menu.clone(),
                        t_for_menu.clone(),
                        is_view,
                    )
                });
                row.into_any_element()
            }
            TreeRow::TablePlaceholder { text, is_error } => {
                render_columns_placeholder(text.clone(), if *is_error { red } else { muted_fg })
            }
            TreeRow::Column { key, column_index } => self
                .table_columns
                .get(key.as_ref())
                .and_then(|columns| columns.columns.get(*column_index))
                .map_or_else(
                    || div().h(px(28.0)).into_any_element(),
                    |column| {
                        render_column_row(
                            column,
                            SharedString::from(format!(
                                "tree-column-copy-{}-{}-{column_index}",
                                key.0, key.1
                            )),
                            fg,
                            muted_fg,
                            cx,
                        )
                    },
                ),
            TreeRow::SectionLabel { text } => render_columns_placeholder(text.clone(), muted_fg),
            TreeRow::DetailLine {
                element_id,
                text,
                copy_value,
            } => render_copyable_detail_line(
                element_id.clone(),
                text.clone(),
                copy_value.clone(),
                fg,
                cx,
            ),
        }
    }
}

fn build_tree_rows(
    schemas: &[Schema],
    expanded: &HashMap<String, SchemaTables>,
    open_schemas: &HashSet<String>,
    table_columns: &HashMap<(String, String), TableColumns>,
    show_system: bool,
    filter: &str,
) -> TreeRowsView {
    let has_filter = !filter.is_empty();
    let mut visible: Vec<&Schema> = schemas
        .iter()
        .filter(|schema| show_system || !is_system_schema(&schema.name))
        .filter(|schema| {
            contains_case_insensitive(&schema.name, filter)
                || expanded.get(&schema.name).is_some_and(|entry| {
                    entry
                        .tables
                        .iter()
                        .any(|table| contains_case_insensitive(&table.name, filter))
                })
        })
        .collect();
    visible.sort_by(|left, right| {
        is_system_schema(&left.name)
            .cmp(&is_system_schema(&right.name))
            .then_with(|| left.name.cmp(&right.name))
    });

    let searchable_schemas = schemas
        .iter()
        .filter(|schema| show_system || !is_system_schema(&schema.name))
        .filter(|schema| {
            expanded
                .get(&schema.name)
                .is_some_and(|entry| !entry.loading && entry.error.is_none())
        })
        .count();
    let failed_schemas = schemas
        .iter()
        .filter(|schema| show_system || !is_system_schema(&schema.name))
        .filter(|schema| {
            expanded
                .get(&schema.name)
                .is_some_and(|entry| entry.error.is_some())
        })
        .count();
    let visible_schemas = visible.len();
    let mut rows = Vec::with_capacity(visible_schemas.saturating_mul(4));

    for schema in visible {
        let name = &schema.name;
        let is_expanded = open_schemas.contains(name) || has_filter;
        rows.push(TreeRow::Schema {
            name: name.clone(),
            is_expanded,
            is_system: is_system_schema(name),
        });

        let Some(schema_tables) = expanded.get(name).filter(|_| is_expanded) else {
            continue;
        };
        if schema_tables.loading {
            rows.push(TreeRow::SchemaPlaceholder {
                text: "加载 tables…".into(),
                is_error: false,
            });
            continue;
        }
        if let Some(error) = &schema_tables.error {
            rows.push(TreeRow::SchemaPlaceholder {
                text: error.clone(),
                is_error: true,
            });
            continue;
        }
        if schema_tables.tables.is_empty() {
            rows.push(TreeRow::SchemaPlaceholder {
                text: "（空）".into(),
                is_error: false,
            });
            continue;
        }

        let total_tables = schema_tables
            .tables
            .iter()
            .filter(|table| !table.is_view)
            .count();
        let total_views = schema_tables
            .tables
            .iter()
            .filter(|table| table.is_view)
            .count();
        let show_group_header = total_tables > 0 && total_views > 0;
        let schema_matches = contains_case_insensitive(name, filter);
        let mut last_was_view = None;
        for table in &schema_tables.tables {
            if has_filter && !schema_matches && !contains_case_insensitive(&table.name, filter) {
                continue;
            }
            if show_group_header && last_was_view != Some(table.is_view) {
                rows.push(TreeRow::GroupHeader {
                    text: if table.is_view {
                        format!("视图 ({total_views})")
                    } else {
                        format!("表 ({total_tables})")
                    },
                });
                last_was_view = Some(table.is_view);
            }

            let columns_key = Rc::new((name.clone(), table.name.clone()));
            let columns = table_columns.get(columns_key.as_ref());
            rows.push(TreeRow::Table {
                key: columns_key.clone(),
                is_view: table.is_view,
                is_cols_expanded: columns.is_some(),
            });

            let Some(columns) = columns else {
                continue;
            };
            if columns.loading {
                rows.push(TreeRow::TablePlaceholder {
                    text: "加载列结构…".into(),
                    is_error: false,
                });
                continue;
            }
            if let Some(error) = &columns.error {
                rows.push(TreeRow::TablePlaceholder {
                    text: format!("加载失败：{error}"),
                    is_error: true,
                });
                continue;
            }

            rows.extend(columns.columns.iter().enumerate().map(|(column_index, _)| {
                TreeRow::Column {
                    key: columns_key.clone(),
                    column_index,
                }
            }));
            if !columns.indexes.is_empty() {
                rows.push(TreeRow::SectionLabel {
                    text: format!("索引 ({})", columns.indexes.len()),
                });
                for index in &columns.indexes {
                    let prefix = if index.primary {
                        "🔑 PK"
                    } else if index.unique {
                        "★ UQ"
                    } else {
                        "·"
                    };
                    rows.push(TreeRow::DetailLine {
                        element_id: SharedString::from(format!(
                            "tree-index-copy-{name}-{}-{}",
                            table.name, index.name
                        )),
                        text: format!("{prefix}  {}({})", index.name, index.columns.join(", ")),
                        copy_value: index.name.clone(),
                    });
                }
            }
            if !columns.foreign_keys.is_empty() {
                rows.push(TreeRow::SectionLabel {
                    text: format!("外键 ({})", columns.foreign_keys.len()),
                });
                for foreign_key in &columns.foreign_keys {
                    rows.push(TreeRow::DetailLine {
                        element_id: SharedString::from(format!(
                            "tree-foreign-key-copy-{name}-{}-{}",
                            table.name, foreign_key.name
                        )),
                        text: format!(
                            "↗ {} ({}) → {}.{}({})",
                            foreign_key.name,
                            foreign_key.columns.join(", "),
                            foreign_key.ref_schema,
                            foreign_key.ref_table,
                            foreign_key.ref_columns.join(", ")
                        ),
                        copy_value: foreign_key.name.clone(),
                    });
                }
            }
        }
    }

    TreeRowsView {
        rows: Rc::new(rows),
        visible_schemas,
        searchable_schemas,
        failed_schemas,
    }
}

#[cfg(test)]
mod tests;
