use super::{
    ToolDragGlobal, ToolDragSurface, ToolDropSide, ToolDropTarget,
    activity_drop_target_from_position, dragged_item_background, home_drop_side,
    home_drop_target_from_position, tool_drag_display_slots, tool_drop_boundary,
};

#[test]
fn dragged_item_background_is_lighter_but_stays_within_range() {
    let background = gpui::hsla(0.6, 0.4, 0.2, 1.0);
    let highlighted = dragged_item_background(background);

    assert!(highlighted.l > background.l);
    assert_eq!(highlighted.a, background.a);

    let bright_background = gpui::hsla(0.6, 0.4, 0.9, 1.0);
    assert_eq!(dragged_item_background(bright_background).l, 1.0);
}

#[test]
fn drop_boundaries_follow_the_requested_edge() {
    assert_eq!(tool_drop_boundary(2, ToolDropSide::Left), 2);
    assert_eq!(tool_drop_boundary(2, ToolDropSide::Top), 2);
    assert_eq!(tool_drop_boundary(2, ToolDropSide::Right), 3);
    assert_eq!(tool_drop_boundary(2, ToolDropSide::Bottom), 3);
}

#[test]
fn home_target_selection_supports_all_four_edges() {
    assert_eq!(
        home_drop_side(20.0, 56.0, 280.0, 112.0, 3, 1),
        ToolDropSide::Left
    );
    assert_eq!(
        home_drop_side(260.0, 56.0, 280.0, 112.0, 0, 1),
        ToolDropSide::Right
    );
    assert_eq!(
        home_drop_side(140.0, 10.0, 280.0, 112.0, 3, 1),
        ToolDropSide::Top
    );
    assert_eq!(
        home_drop_side(140.0, 102.0, 280.0, 112.0, 0, 1),
        ToolDropSide::Bottom
    );
}

#[test]
fn home_target_selection_uses_the_gap_after_a_card() {
    assert_eq!(
        home_drop_target_from_position(
            288.0,
            56.0,
            3,
            super::HomeDropLayout {
                width: 280.0,
                height: 112.0,
                item_count: 4,
                columns: 3,
                gap: 16.0,
            },
        ),
        Some((0, ToolDropSide::Right))
    );
}

#[test]
fn activity_target_selection_only_uses_horizontal_lines() {
    assert_eq!(
        activity_drop_target_from_position(60.0, 3),
        Some((1, ToolDropSide::Top))
    );
    assert_eq!(
        activity_drop_target_from_position(86.0, 3),
        Some((1, ToolDropSide::Bottom))
    );
}

#[test]
fn display_slots_keep_every_card_while_dragging() {
    let ids = ["a", "b", "c", "d"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let state = ToolDragGlobal {
        dragged_id: Some("b".to_owned()),
        source_index: 1,
        target: Some(ToolDropTarget {
            surface: ToolDragSurface::Home,
            index: 2,
            side: ToolDropSide::Right,
        }),
        revision: 1,
    };

    assert_eq!(
        tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
        vec![
            Some("a".to_owned()),
            Some("b".to_owned()),
            Some("c".to_owned()),
            Some("d".to_owned())
        ]
    );
}

#[test]
fn display_slots_keep_the_source_card_during_cross_surface_drag() {
    let ids = ["a", "b", "c"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let state = ToolDragGlobal {
        dragged_id: Some("b".to_owned()),
        source_index: 1,
        target: Some(ToolDropTarget {
            surface: ToolDragSurface::ActivityBar,
            index: 0,
            side: ToolDropSide::Top,
        }),
        revision: 1,
    };

    assert_eq!(
        tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
        vec![
            Some("a".to_owned()),
            Some("b".to_owned()),
            Some("c".to_owned())
        ]
    );
    assert_eq!(
        tool_drag_display_slots(&ids, ToolDragSurface::ActivityBar, &state),
        vec![
            Some("a".to_owned()),
            Some("b".to_owned()),
            Some("c".to_owned())
        ]
    );
}

#[test]
fn display_slots_keep_the_source_card_before_the_first_move() {
    let ids = ["a", "b", "c"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let state = ToolDragGlobal {
        dragged_id: Some("b".to_owned()),
        source_index: 1,
        target: Some(ToolDropTarget {
            surface: ToolDragSurface::Home,
            index: 1,
            side: ToolDropSide::Left,
        }),
        revision: 1,
    };

    assert_eq!(
        tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
        vec![
            Some("a".to_owned()),
            Some("b".to_owned()),
            Some("c".to_owned())
        ]
    );
}

#[test]
fn display_slots_keep_every_card_when_moving_to_an_earlier_target() {
    let ids = ["a", "b", "c", "d"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let state = ToolDragGlobal {
        dragged_id: Some("d".to_owned()),
        source_index: 3,
        target: Some(ToolDropTarget {
            surface: ToolDragSurface::Home,
            index: 1,
            side: ToolDropSide::Left,
        }),
        revision: 1,
    };

    assert_eq!(
        tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
        vec![
            Some("a".to_owned()),
            Some("b".to_owned()),
            Some("c".to_owned()),
            Some("d".to_owned())
        ]
    );
}
