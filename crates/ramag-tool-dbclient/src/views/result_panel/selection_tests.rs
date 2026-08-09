use super::{toggle_visible_selection, visible_selection_count};
use std::collections::BTreeSet;

#[test]
fn filtered_select_all_uses_source_row_indices() {
    let mut selected = BTreeSet::new();

    toggle_visible_selection(&mut selected, &[2]);

    assert_eq!(selected, BTreeSet::from([2]));
}

#[test]
fn toggling_visible_rows_preserves_hidden_selection() {
    let mut selected = BTreeSet::from([0, 2, 4]);

    toggle_visible_selection(&mut selected, &[2, 4]);

    assert_eq!(selected, BTreeSet::from([0]));
}

#[test]
fn partial_visible_selection_selects_remaining_visible_rows() {
    let mut selected = BTreeSet::from([2]);

    toggle_visible_selection(&mut selected, &[2, 4]);

    assert_eq!(selected, BTreeSet::from([2, 4]));
}

#[test]
fn visible_selection_count_ignores_hidden_rows() {
    let selected = BTreeSet::from([0, 2, 4]);

    assert_eq!(visible_selection_count(&selected, &[2, 3, 4]), 2);
}
