use super::active_index_after_close;

#[test]
fn closing_tab_left_of_active_preserves_the_same_logical_tab() {
    assert_eq!(active_index_after_close(1, 0, 2), 0);
}

#[test]
fn closing_active_last_tab_activates_new_last_tab() {
    assert_eq!(active_index_after_close(2, 2, 2), 1);
}

#[test]
fn closing_tab_right_of_active_keeps_index() {
    assert_eq!(active_index_after_close(0, 2, 2), 0);
}
