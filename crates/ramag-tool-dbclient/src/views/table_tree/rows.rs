//! 表树可见行构造与本地筛选。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::SharedString;
use ramag_domain::entities::{Schema, contains_case_insensitive};

use super::navigation::{
    TableNavigationRef, TableTreeFilter, schema_has_navigation_item, table_matches_filter,
};
use super::row::{TreeRow, TreeRowsView};
use super::{SchemaTables, TableColumns, TableTreeNavigation, TableTreeSection};
use crate::sql_completion::is_system_schema;

#[cfg(test)]
pub(super) fn build_tree_rows(
    schemas: &[Schema],
    expanded: &HashMap<String, SchemaTables>,
    open_schemas: &HashSet<String>,
    table_columns: &HashMap<(String, String), TableColumns>,
    show_system: bool,
    filter: &str,
) -> TreeRowsView {
    build_tree_rows_with_navigation(
        schemas,
        expanded,
        open_schemas,
        table_columns,
        show_system,
        filter,
        TableTreeNavigation {
            table_filter: TableTreeFilter::All,
            connection_id: None,
            navigation_favorites: &HashSet::new(),
            recent_tables: &[],
        },
    )
}

// 根据元数据、展开状态和导航条件构建扁平可见行，并统计搜索与失败状态。
pub(super) fn build_tree_rows_with_navigation(
    schemas: &[Schema],
    expanded: &HashMap<String, SchemaTables>,
    open_schemas: &HashSet<String>,
    table_columns: &HashMap<(String, String), TableColumns>,
    show_system: bool,
    filter: &str,
    navigation: TableTreeNavigation<'_>,
) -> TreeRowsView {
    let TableTreeNavigation {
        table_filter,
        connection_id,
        navigation_favorites,
        recent_tables,
    } = navigation;
    let has_filter = !filter.is_empty();
    let mut visible: Vec<&Schema> = schemas
        .iter()
        .filter(|schema| show_system || !is_system_schema(&schema.name))
        .filter(|schema| {
            if table_filter != TableTreeFilter::All
                && !schema_has_navigation_item(
                    table_filter,
                    connection_id,
                    &schema.name,
                    navigation_favorites,
                    recent_tables,
                )
            {
                return false;
            }
            !has_filter
                || contains_case_insensitive(&schema.name, filter)
                || expanded.get(&schema.name).is_some_and(|entry| {
                    entry.tables.iter().any(|table| {
                        table_matches_filter(
                            table_filter,
                            connection_id,
                            &schema.name,
                            &table.name,
                            navigation_favorites,
                            recent_tables,
                        ) && contains_case_insensitive(&table.name, filter)
                    })
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
        let is_expanded =
            open_schemas.contains(name) || has_filter || table_filter != TableTreeFilter::All;
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
            if !table_matches_filter(
                table_filter,
                connection_id,
                name,
                &table.name,
                navigation_favorites,
                recent_tables,
            ) {
                continue;
            }
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
                is_favorite: connection_id.is_some_and(|connection_id| {
                    navigation_favorites.contains(&TableNavigationRef {
                        connection_id: connection_id.clone(),
                        schema: name.clone(),
                        table: table.name.clone(),
                    })
                }),
                size_bytes: table.size_bytes,
            });

            let Some(columns) = columns else {
                continue;
            };
            if columns.loading {
                rows.push(TreeRow::TablePlaceholder {
                    text: "加载表结构…".into(),
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
            let key_count = columns
                .indexes
                .iter()
                .filter(|index| index.primary || index.unique)
                .count();
            if key_count > 0 {
                rows.push(TreeRow::Section {
                    key: columns_key.clone(),
                    section: TableTreeSection::Keys,
                    text: "键".into(),
                    count: key_count,
                    is_expanded: columns.sections.keys,
                });
                if columns.sections.keys {
                    for index in columns
                        .indexes
                        .iter()
                        .filter(|index| index.primary || index.unique)
                    {
                        let prefix = if index.primary { "🔑 PK" } else { "★ UQ" };
                        rows.push(TreeRow::DetailLine {
                            element_id: SharedString::from(format!(
                                "tree-key-copy-{name}-{}-{}",
                                table.name, index.name
                            )),
                            text: format!("{prefix}  {}({})", index.name, index.columns.join(", ")),
                            copy_value: index.name.clone(),
                        });
                    }
                }
            }
            let index_count = columns
                .indexes
                .iter()
                .filter(|index| !index.primary && !index.unique)
                .count();
            if index_count > 0 {
                rows.push(TreeRow::Section {
                    key: columns_key.clone(),
                    section: TableTreeSection::Indexes,
                    text: "索引".into(),
                    count: index_count,
                    is_expanded: columns.sections.indexes,
                });
                if columns.sections.indexes {
                    for (index_index, index) in columns
                        .indexes
                        .iter()
                        .enumerate()
                        .filter(|(_, index)| !index.primary && !index.unique)
                    {
                        rows.push(TreeRow::Index {
                            key: columns_key.clone(),
                            index_index,
                        });
                    }
                }
            }
            if !columns.foreign_keys.is_empty() {
                rows.push(TreeRow::Section {
                    key: columns_key.clone(),
                    section: TableTreeSection::ForeignKeys,
                    text: "外键".into(),
                    count: columns.foreign_keys.len(),
                    is_expanded: columns.sections.foreign_keys,
                });
                if columns.sections.foreign_keys {
                    for foreign_key in &columns.foreign_keys {
                        rows.push(TreeRow::DetailLine {
                            element_id: SharedString::from(format!(
                                "tree-foreign-key-copy-{name}-{}-{}",
                                table.name, foreign_key.name
                            )),
                            text: format!(
                                "↗ {} ({}) → {}.{}({}) [ON DELETE {}, ON UPDATE {}]",
                                foreign_key.name,
                                foreign_key.columns.join(", "),
                                foreign_key.ref_schema,
                                foreign_key.ref_table,
                                foreign_key.ref_columns.join(", "),
                                foreign_key.on_delete.as_sql(),
                                foreign_key.on_update.as_sql()
                            ),
                            copy_value: foreign_key.name.clone(),
                        });
                    }
                }
            }
            if !columns.triggers.is_empty() {
                rows.push(TreeRow::Section {
                    key: columns_key.clone(),
                    section: TableTreeSection::Triggers,
                    text: "触发器".into(),
                    count: columns.triggers.len(),
                    is_expanded: columns.sections.triggers,
                });
                if columns.sections.triggers {
                    for (trigger_index, _) in columns.triggers.iter().enumerate() {
                        rows.push(TreeRow::Trigger {
                            key: columns_key.clone(),
                            trigger_index,
                        });
                    }
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
