#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gpui::TestAppContext;
use ramag_domain::entities::{QueryResult, Row, Value};

use super::parser::parse_plan;
use super::*;
use crate::views::result_panel::{ResultPanel, ResultState};

fn result(columns: &[&str], rows: Vec<Vec<Value>>) -> QueryResult {
    QueryResult {
        columns: columns.iter().map(|column| (*column).into()).collect(),
        column_types: vec!["TEXT".into(); columns.len()],
        rows: rows.into_iter().map(|values| Row { values }).collect(),
        affected_rows: 0,
        elapsed_ms: 1,
        warnings: Vec::new(),
        truncated: false,
    }
}

#[test]
fn parses_mysql_explain_rows_into_id_tree() {
    let result = result(
        &[
            "id",
            "select_type",
            "table",
            "type",
            "possible_keys",
            "key",
            "rows",
            "Extra",
        ],
        vec![
            vec![
                Value::Int(1),
                Value::Text("SIMPLE".into()),
                Value::Text("orders".into()),
                Value::Text("ALL".into()),
                Value::Text("idx_user".into()),
                Value::Null,
                Value::Int(100),
                Value::Text("Using where".into()),
            ],
            vec![
                Value::Int(2),
                Value::Text("SUBQUERY".into()),
                Value::Text("users".into()),
                Value::Text("ref".into()),
                Value::Null,
                Value::Text("PRIMARY".into()),
                Value::Int(1),
                Value::Null,
            ],
        ],
    );

    let tree = parse_plan(&result).expect("MySQL plan should parse");
    assert_eq!(tree.source, PlanSource::Mysql);
    assert_eq!(tree.rows.len(), 2);
    assert_eq!(tree.rows[0].label, "SIMPLE · orders · ALL");
    assert_eq!(tree.rows[1].parent, Some(0));
    assert_eq!(tree.rows[1].depth, 1);
    assert!(tree.rows[0].detail.as_deref().is_some_and(
        |detail| detail.contains("key=PRIMARY") || detail.contains("Extra=Using where")
    ));
}

#[test]
fn parses_postgres_text_plan_and_details() {
    let plan_result = result(
        &["QUERY PLAN"],
        vec![
            vec![Value::Text(
                "Nested Loop  (cost=0.00..12.50 rows=2 width=8)".into(),
            )],
            vec![Value::Text(
                "  ->  Index Scan using users_pkey on users".into(),
            )],
            vec![Value::Text("        Index Cond: (id = 1)".into())],
            vec![Value::Text("  ->  Seq Scan on orders".into())],
            vec![Value::Text("        Filter: (status = 'open')".into())],
            vec![Value::Text("Planning Time: 0.120 ms".into())],
        ],
    );

    let tree = parse_plan(&plan_result).expect("PostgreSQL plan should parse");
    assert_eq!(tree.source, PlanSource::Postgres);
    assert_eq!(
        tree.rows[0].label,
        "Nested Loop  (cost=0.00..12.50 rows=2 width=8)"
    );
    assert_eq!(tree.rows[1].parent, Some(0));
    assert_eq!(tree.rows[1].depth, 1);
    assert_eq!(tree.rows[2].parent, Some(1));
    assert_eq!(tree.rows[2].label, "Index Cond");
    assert_eq!(tree.rows[2].detail.as_deref(), Some("(id = 1)"));
    assert_eq!(tree.rows[3].parent, Some(0));
    assert_eq!(tree.rows[5].parent, Some(3));

    let costs_off = result(
        &["QUERY PLAN"],
        vec![
            vec![Value::Text("Limit".into())],
            vec![Value::Text(
                "  ->  Index Only Scan using users_pkey on users".into(),
            )],
        ],
    );
    let costs_off_tree = parse_plan(&costs_off).expect("costless PostgreSQL plan should parse");
    assert_eq!(costs_off_tree.rows.len(), 2);
    assert_eq!(costs_off_tree.rows[1].parent, Some(0));
    assert_eq!(costs_off_tree.rows[1].depth, 1);
}

#[test]
fn does_not_treat_a_regular_single_column_result_as_a_plan() {
    let regular_result = result(
        &["QUERY PLAN"],
        vec![vec![Value::Text("ordinary application value".into())]],
    );
    assert!(parse_plan(&regular_result).is_none());

    let other_result = result(&["value"], vec![vec![Value::Text("Seq Scan".into())]]);
    assert!(parse_plan(&other_result).is_none());
}

#[test]
fn collapsed_rows_hide_only_descendants() {
    let rows = vec![
        PlanRow {
            id: 0,
            parent: None,
            depth: 0,
            label: "root".into(),
            detail: None,
            is_detail: false,
            has_children: true,
        },
        PlanRow {
            id: 1,
            parent: Some(0),
            depth: 1,
            label: "child".into(),
            detail: None,
            is_detail: false,
            has_children: false,
        },
        PlanRow {
            id: 2,
            parent: None,
            depth: 0,
            label: "sibling root".into(),
            detail: None,
            is_detail: false,
            has_children: false,
        },
    ];
    let visible = visible_plan_indices(&rows, &BTreeSet::from([0]));
    assert_eq!(visible, vec![0, 2]);
}

#[gpui::test]
fn renders_structured_plan_tree_and_keeps_it_read_only(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let result = Arc::new(result(
        &["id", "select_type", "table", "type", "rows"],
        vec![vec![
            Value::Int(1),
            Value::Text("SIMPLE".into()),
            Value::Text("users".into()),
            Value::Text("ALL".into()),
            Value::Int(10),
        ]],
    ));
    let mut panel_entity = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| {
            let mut panel = ResultPanel::new(window, cx);
            panel.set_plan_mode(true);
            panel.state = ResultState::Ok(result);
            panel
        });
        panel_entity = Some(panel.clone());
        gpui_component::Root::new(panel, window, cx)
    });
    let panel = panel_entity.expect("result panel should be initialized");
    panel.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("plan-tree-horizontal-scroll").is_some(),
        "structured plan view should render its scrollable body"
    );
    assert!(
        cx.debug_bounds("plan-tree-vertical-scrollbar").is_some(),
        "structured plan view should expose a vertical scrollbar"
    );
    assert!(
        cx.debug_bounds("plan-tree-horizontal-scrollbar").is_some(),
        "structured plan view should expose a horizontal scrollbar"
    );
    panel.read_with(cx, |panel, _| {
        assert_eq!(
            panel.insert_block_reason(),
            Some("执行计划只读，仅可查看和复制")
        );
        assert!(panel.cell_edit_block_reason(0, 0).is_some());
    });
}
