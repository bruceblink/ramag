//! 数据库对象树行。

use std::rc::Rc;

#[cfg(test)]
use std::collections::{HashMap, HashSet};

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
#[cfg(test)]
use ramag_domain::entities::Schema;
use ramag_domain::entities::format_bytes;

#[cfg(test)]
use super::{SchemaTables, TableColumns};
use super::{TableTreeNavigation, TableTreePanel, TableTreeSection, navigation::TableTreeFilter};
use crate::views::tree_helpers::{
    render_column_row, render_columns_placeholder, render_copyable_detail_line,
};

#[cfg(test)]
use super::rows::build_tree_rows;
use super::rows::build_tree_rows_with_navigation;

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
        is_favorite: bool,
        size_bytes: Option<u64>,
    },
    TablePlaceholder {
        text: String,
        is_error: bool,
    },
    Column {
        key: Rc<(String, String)>,
        column_index: usize,
    },
    Section {
        key: Rc<(String, String)>,
        section: TableTreeSection,
        text: String,
        count: usize,
        is_expanded: bool,
    },
    Index {
        key: Rc<(String, String)>,
        index_index: usize,
    },
    Trigger {
        key: Rc<(String, String)>,
        trigger_index: usize,
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
    table_filter: TableTreeFilter,
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
    pub(super) fn tree_rows_view(&self, filter: &str) -> TreeRowsView {
        let key = TreeRowsCacheKey {
            tree_revision: self.tree_revision,
            show_system: self.show_system,
            filter: filter.to_string(),
            table_filter: self.table_filter,
        };
        {
            let cache = self.tree_rows_cache.borrow();
            if let Some(view) = cache.as_ref().and_then(|entry| entry.get(&key)) {
                return view;
            }
        }

        let view = build_tree_rows_with_navigation(
            &self.schemas,
            &self.expanded,
            &self.open_schemas,
            &self.table_columns,
            self.show_system,
            filter,
            TableTreeNavigation {
                table_filter: self.table_filter,
                connection_id: self.connection.as_ref().map(|connection| &connection.id),
                navigation_favorites: &self.navigation_favorites,
                recent_tables: &self.recent_tables,
            },
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
                let chevron = if *is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
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
                            .w(px(14.0))
                            .child(Icon::new(chevron).xsmall().text_color(muted_fg)),
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
                is_favorite,
                size_bytes,
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
                let is_favorite = *is_favorite;
                let size_label = size_bytes.map(format_bytes);

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
                                        "收起表结构"
                                    } else {
                                        "展开表结构"
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
                    )
                    .when(is_favorite, |row| {
                        row.child(
                            Icon::new(IconName::StarFill)
                                .xsmall()
                                .text_color(if is_selected { accent_fg } else { muted_fg }),
                        )
                    })
                    .when_some(size_label, |row, size| {
                        row.child(
                            div()
                                .ml_auto()
                                .flex_none()
                                .px(px(3.0))
                                .rounded(px(2.0))
                                .border_1()
                                .border_color(muted_fg.opacity(0.35))
                                .bg(muted_bg)
                                .text_xs()
                                .text_color(if is_selected { accent_fg } else { fg })
                                .child(size),
                        )
                    });
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
                        is_favorite,
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
            TreeRow::Section {
                key,
                section,
                text,
                count,
                is_expanded,
            } => {
                let section = *section;
                let row_id =
                    SharedString::from(format!("table-section-{}-{}-{section:?}", key.0, key.1));
                let schema = key.0.clone();
                let table = key.1.clone();
                let chevron = if *is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                h_flex()
                    .id(row_id)
                    .w_full()
                    .h(px(28.0))
                    .flex_none()
                    .items_center()
                    .gap(px(4.0))
                    .pl(px(40.0))
                    .pr_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |this| this.bg(muted_bg))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_table_section(schema.clone(), table.clone(), section, cx);
                    }))
                    .child(Icon::new(chevron).xsmall().text_color(muted_fg))
                    .child(
                        Icon::new(section_icon(section))
                            .xsmall()
                            .text_color(muted_fg),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(fg)
                            .whitespace_nowrap()
                            .child(format!("{} ({count})", text)),
                    )
                    .into_any_element()
            }
            TreeRow::Index { key, index_index } => self
                .table_columns
                .get(key.as_ref())
                .and_then(|columns| columns.indexes.get(*index_index))
                .map_or_else(
                    || div().h(px(28.0)).into_any_element(),
                    |index| {
                        super::metadata_ops::render_index_row(
                            index,
                            key.0.clone(),
                            key.1.clone(),
                            *index_index,
                            fg,
                            cx,
                        )
                    },
                ),
            TreeRow::Trigger { key, trigger_index } => self
                .table_columns
                .get(key.as_ref())
                .and_then(|columns| columns.triggers.get(*trigger_index))
                .map_or_else(
                    || div().h(px(28.0)).into_any_element(),
                    |trigger| {
                        super::metadata_ops::render_trigger_row(
                            trigger,
                            key.0.clone(),
                            key.1.clone(),
                            *trigger_index,
                            fg,
                            cx,
                        )
                    },
                ),
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

fn section_icon(section: TableTreeSection) -> IconName {
    match section {
        TableTreeSection::Keys => IconName::File,
        TableTreeSection::Indexes => IconName::File,
        TableTreeSection::ForeignKeys => IconName::ArrowRight,
        TableTreeSection::Triggers => IconName::Network,
    }
}

#[cfg(test)]
mod tests;
