//! MongoDB 结果面板状态与行视图测试。

use super::cell::Cell;
use super::flatten::Column;
use super::*;

fn table() -> FlatTable {
    FlatTable {
        columns: vec![
            Column {
                path: "name".into(),
                kind: "text",
            },
            Column {
                path: "n".into(),
                kind: "int",
            },
        ],
        total_columns: 2,
        rows: [("Bob", "10"), ("Alice", "2"), ("Bobby", "1")]
            .into_iter()
            .map(|(name, number)| {
                vec![
                    Cell {
                        text: name.into(),
                        kind: "text",
                    },
                    Cell {
                        text: number.into(),
                        kind: "int",
                    },
                ]
            })
            .collect(),
    }
}

#[test]
fn row_view_combines_filter_and_numeric_sort() {
    let key = RowViewKey {
        generation: 1,
        filter: RowFilter::Text("bo".into()),
        sort_by: Some(("n".into(), SortDir::Desc)),
    };

    let cancelled = AtomicBool::new(false);
    let indices = build_row_view_indices(&table(), &key, &cancelled).unwrap();

    assert_eq!(indices.as_slice(), &[0, 2]);
}

#[test]
fn row_view_stops_before_scanning_when_cancelled() {
    let key = RowViewKey {
        generation: 1,
        filter: RowFilter::Text("bo".into()),
        sort_by: None,
    };
    let cancelled = AtomicBool::new(true);

    assert!(build_row_view_indices(&table(), &key, &cancelled).is_none());
}

#[test]
fn row_view_cache_key_changes_with_generation() {
    let key = RowViewKey {
        generation: 2,
        filter: RowFilter::Text(String::new()),
        sort_by: None,
    };
    let cache = RowViewCache {
        key: key.clone(),
        indices: Arc::new(vec![0, 1]),
    };
    assert_eq!(cache.key, key);

    let mut stale = key;
    stale.generation += 1;
    assert_ne!(cache.key, stale);
}

#[test]
fn row_view_cache_key_changes_with_search_mode() {
    let text = RowViewKey {
        generation: 2,
        filter: RowFilter::Text("82489".into()),
        sort_by: None,
    };
    let integer = RowViewKey {
        filter: RowFilter::Integer(82_489),
        ..text.clone()
    };

    assert_ne!(text, integer);
}

#[test]
fn visible_selection_count_ignores_hidden_rows() {
    let selected = BTreeSet::from([0, 2, 4]);

    assert_eq!(visible_selection_count(&selected, &[2, 3, 4]), 2);
}

#[test]
fn cell_dialog_text_is_unicode_safe_and_bounded() {
    let display = bounded_cell_dialog_text("你".repeat(40), 64);

    assert!(display.len() <= 64);
    assert!(display.is_char_boundary(display.len()));
    assert!(display.contains("[内容过大"));
}
