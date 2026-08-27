use super::{
    ClosedQueryDraft, MAX_CLOSED_QUERY_DRAFTS, active_index_after_close, push_closed_draft,
};

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

#[test]
fn closed_draft_stack_is_bounded_and_reopens_newest_first() {
    let mut stack = Vec::new();
    for index in 0..=MAX_CLOSED_QUERY_DRAFTS {
        push_closed_draft(
            &mut stack,
            ClosedQueryDraft {
                title: format!("查询 {index}"),
                text: format!("SELECT {index}").into(),
                context: None,
            },
        );
    }

    assert_eq!(stack.len(), MAX_CLOSED_QUERY_DRAFTS);
    assert_eq!(
        stack.first().map(|draft| draft.title.as_str()),
        Some("查询 1")
    );
    assert_eq!(stack.pop().map(|draft| draft.title), Some("查询 10".into()));
}
