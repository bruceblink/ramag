use ramag_domain::entities::{
    Column, ColumnKind, ColumnType, ConnectionId, ForeignKey, ForeignKeyAction, Index, Table,
    Trigger,
};

use super::*;
use crate::views::table_tree::navigation::{TableNavigationRef, TableTreeFilter};

#[test]
fn metadata_rows_keep_exact_copy_targets() {
    let schemas = vec![Schema {
        name: "gewu".into(),
        charset: None,
        collation: None,
    }];
    let expanded = HashMap::from([(
        "gewu".into(),
        SchemaTables {
            tables: vec![Table {
                name: "company_project_member_rel".into(),
                schema: "gewu".into(),
                comment: None,
                is_view: false,
                size_bytes: Some(12_345),
            }],
            ..Default::default()
        },
    )]);
    let mut table_columns = HashMap::from([(
        ("gewu".into(), "company_project_member_rel".into()),
        TableColumns {
            columns: vec![Column {
                name: "project_id".into(),
                data_type: ColumnType {
                    kind: ColumnKind::Integer,
                    raw_type: "bigint".into(),
                },
                nullable: false,
                default_value: None,
                is_primary_key: false,
                comment: None,
                ordinal_position: None,
                is_auto_increment: false,
                generation_expression: None,
                generated_storage: None,
                identity_generation: None,
            }],
            indexes: vec![
                Index {
                    name: "uk_project_user".into(),
                    unique: true,
                    primary: false,
                    columns: vec!["project_id".into(), "user_id".into()],
                },
                Index {
                    name: "idx_project_id".into(),
                    unique: false,
                    primary: false,
                    columns: vec!["project_id".into()],
                },
            ],
            foreign_keys: vec![ForeignKey {
                name: "fk_project".into(),
                columns: vec!["project_id".into()],
                ref_schema: "gewu".into(),
                ref_table: "company_project".into(),
                ref_columns: vec!["id".into()],
                on_delete: ForeignKeyAction::Cascade,
                on_update: ForeignKeyAction::SetNull,
            }],
            triggers: vec![Trigger {
                name: "trg_project_audit".into(),
                timing: "BEFORE".into(),
                event: "INSERT".into(),
                definition: "BEGIN END".into(),
            }],
            ..Default::default()
        },
    )]);

    let view = build_tree_rows(
        &schemas,
        &expanded,
        &HashSet::from(["gewu".into()]),
        &table_columns,
        false,
        "",
    );

    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::Column { key, column_index: 0 }
            if key.0 == "gewu" && key.1 == "company_project_member_rel")
    }));
    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::Table { key, size_bytes: Some(12_345), .. }
            if key.0 == "gewu" && key.1 == "company_project_member_rel")
    }));
    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::DetailLine { copy_value, .. }
            if copy_value == "uk_project_user")
    }));
    assert!(
        view.rows
            .iter()
            .any(|row| { matches!(row, TreeRow::Section { text, count: 1, .. } if text == "键") })
    );
    assert!(
        view.rows.iter().any(|row| {
            matches!(row, TreeRow::Section { text, count: 1, .. } if text == "索引")
        })
    );
    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::Index { key, index_index: 1 }
            if key.0 == "gewu" && key.1 == "company_project_member_rel")
    }));
    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::DetailLine { copy_value, .. }
            if copy_value == "fk_project")
    }));
    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::DetailLine { text, .. }
            if text.contains("ON DELETE CASCADE") && text.contains("ON UPDATE SET NULL"))
    }));
    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::Section { text, count: 1, .. } if text == "触发器")
    }));
    assert!(view.rows.iter().any(|row| {
        matches!(row, TreeRow::Trigger { key, trigger_index: 0 }
            if key.0 == "gewu" && key.1 == "company_project_member_rel")
    }));

    table_columns
        .get_mut(&("gewu".into(), "company_project_member_rel".into()))
        .expect("test table metadata")
        .sections
        .keys = false;
    table_columns
        .get_mut(&("gewu".into(), "company_project_member_rel".into()))
        .expect("test table metadata")
        .sections
        .indexes = false;
    let collapsed = build_tree_rows(
        &schemas,
        &expanded,
        &HashSet::from(["gewu".into()]),
        &table_columns,
        false,
        "",
    );
    assert!(collapsed.rows.iter().any(|row| {
        matches!(
            row,
            TreeRow::Section {
                is_expanded: false,
                ..
            }
        )
    }));
    assert!(!collapsed.rows.iter().any(|row| {
        matches!(row, TreeRow::DetailLine { copy_value, .. } if copy_value == "uk_project_user")
    }));
    assert!(
        !collapsed
            .rows
            .iter()
            .any(|row| matches!(row, TreeRow::Index { .. }))
    );
}

#[test]
fn tree_rows_match_unicode_table_names_without_lowercase_copies() {
    let schemas = vec![Schema {
        name: "public".into(),
        charset: None,
        collation: None,
    }];
    let expanded = HashMap::from([(
        "public".into(),
        SchemaTables {
            tables: vec![Table {
                name: "ÜBERblick".into(),
                schema: "public".into(),
                comment: None,
                is_view: false,
                size_bytes: None,
            }],
            ..Default::default()
        },
    )]);

    let view = build_tree_rows(
        &schemas,
        &expanded,
        &HashSet::new(),
        &HashMap::new(),
        false,
        "über",
    );

    assert_eq!(view.visible_schemas, 1);
    assert!(
        view.rows
            .iter()
            .any(|row| { matches!(row, TreeRow::Table { key, .. } if key.1 == "ÜBERblick") })
    );
}

#[test]
fn hidden_system_schemas_are_not_counted_in_search_progress() {
    let schemas = vec![
        Schema {
            name: "public".into(),
            charset: None,
            collation: None,
        },
        Schema {
            name: "pg_catalog".into(),
            charset: None,
            collation: None,
        },
    ];
    let expanded = HashMap::from([(
        "public".into(),
        SchemaTables {
            tables: Vec::new(),
            ..Default::default()
        },
    )]);

    let hidden = build_tree_rows(
        &schemas,
        &expanded,
        &HashSet::new(),
        &HashMap::new(),
        false,
        "users",
    );
    assert_eq!(hidden.searchable_schemas, 1);

    let shown = build_tree_rows(
        &schemas,
        &expanded,
        &HashSet::new(),
        &HashMap::new(),
        true,
        "users",
    );
    assert_eq!(shown.searchable_schemas, 1);
}

#[test]
fn navigation_filter_keeps_only_current_connection_tables() {
    let connection_id = ConnectionId::new();
    let other_connection_id = ConnectionId::new();
    let schemas = vec![Schema {
        name: "public".into(),
        charset: None,
        collation: None,
    }];
    let expanded = HashMap::from([(
        "public".into(),
        SchemaTables {
            tables: vec![
                Table {
                    name: "users".into(),
                    schema: "public".into(),
                    comment: None,
                    is_view: false,
                    size_bytes: None,
                },
                Table {
                    name: "orders".into(),
                    schema: "public".into(),
                    comment: None,
                    is_view: false,
                    size_bytes: None,
                },
            ],
            ..Default::default()
        },
    )]);
    let favorites = HashSet::from([TableNavigationRef {
        connection_id: other_connection_id,
        schema: "public".into(),
        table: "users".into(),
    }]);
    let recent = vec![TableNavigationRef {
        connection_id: connection_id.clone(),
        schema: "public".into(),
        table: "orders".into(),
    }];

    let view = build_tree_rows_with_navigation(
        &schemas,
        &expanded,
        &HashSet::new(),
        &HashMap::new(),
        false,
        "",
        TableTreeNavigation {
            table_filter: TableTreeFilter::Favorites,
            connection_id: Some(&connection_id),
            navigation_favorites: &favorites,
            recent_tables: &recent,
        },
    );
    assert!(
        !view
            .rows
            .iter()
            .any(|row| { matches!(row, TreeRow::Table { key, .. } if key.1 == "users") })
    );

    let view = build_tree_rows_with_navigation(
        &schemas,
        &expanded,
        &HashSet::new(),
        &HashMap::new(),
        false,
        "",
        TableTreeNavigation {
            table_filter: TableTreeFilter::Recent,
            connection_id: Some(&connection_id),
            navigation_favorites: &favorites,
            recent_tables: &recent,
        },
    );
    assert!(
        view.rows
            .iter()
            .any(|row| { matches!(row, TreeRow::Table { key, .. } if key.1 == "orders") })
    );
    assert!(
        !view
            .rows
            .iter()
            .any(|row| { matches!(row, TreeRow::Table { key, .. } if key.1 == "users") })
    );
}
