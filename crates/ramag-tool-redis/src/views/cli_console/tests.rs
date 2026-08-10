use super::{
    Entry, MAX_TRANSCRIPT_ENTRIES, MAX_TRANSCRIPT_LINES, Outcome, clear_completed_entries,
    command_preview, next_cursor, outcome_line_count, pending_command_count, prev_cursor,
    prune_transcript_entries, push_command_history, redis_value_retained_bytes,
    split_display_lines, transcript_line_count,
};
use std::collections::VecDeque;

fn ok_lines(text: &str) -> Outcome {
    Outcome::Ok(std::sync::Arc::new(split_display_lines(text)))
}

#[test]
fn prev_from_live_jumps_to_newest() {
    // 实时行首按 ↑ → 最新一条（末尾）
    assert_eq!(prev_cursor(3, None), Some(2));
}

#[test]
fn prev_walks_back_and_stops_at_oldest() {
    assert_eq!(prev_cursor(3, Some(2)), Some(1));
    assert_eq!(prev_cursor(3, Some(1)), Some(0));
    assert_eq!(prev_cursor(3, Some(0)), Some(0)); // 到最旧停住，不越界
}

#[test]
fn prev_empty_history_is_noop() {
    assert_eq!(prev_cursor(0, None), None);
}

#[test]
fn next_walks_forward_then_returns_to_live() {
    assert_eq!(next_cursor(3, 0), Some(1));
    assert_eq!(next_cursor(3, 1), Some(2));
    assert_eq!(next_cursor(3, 2), None); // 越过最新 → 回到实时行
}

#[test]
fn pruning_never_removes_the_latest_entry_even_over_budget() {
    // 续展开可让单条超总行数预算；最新一条（当前结果）必须保留
    let outcome = ok_lines("x");
    let mut entries = vec![Entry {
        id: 1,
        command: "GET big".into(),
        db: 0,
        display_lines: MAX_TRANSCRIPT_LINES + 10,
        outcome,
        elapsed_ms: 1,
        raw: None,
        cursor: None,
    }];
    prune_transcript_entries(&mut entries);
    assert_eq!(entries.len(), 1);
}

#[test]
fn transcript_pruning_is_bounded_and_preserves_pending_entries() {
    let mut entries: Vec<_> = (0..=MAX_TRANSCRIPT_ENTRIES)
        .map(|id| Entry {
            id: id as u64,
            command: "PING".into(),
            db: 0,
            outcome: ok_lines("PONG"),
            display_lines: 1,
            elapsed_ms: 1,
            raw: None,
            cursor: None,
        })
        .collect();
    entries[0].outcome = Outcome::Pending;

    prune_transcript_entries(&mut entries);

    assert_eq!(entries.len(), MAX_TRANSCRIPT_ENTRIES);
    assert!(entries.iter().any(|entry| entry.id == 0));
}

#[test]
fn transcript_pruning_bounds_total_rendered_lines() {
    let line_count = MAX_TRANSCRIPT_LINES / 2 + 1;
    let payload = std::iter::repeat_n("x", line_count)
        .collect::<Vec<_>>()
        .join("\n");
    let mut entries: Vec<_> = (0..3)
        .map(|id| {
            let outcome = ok_lines(&payload);
            Entry {
                id,
                command: "LRANGE queue 0 -1".into(),
                db: 0,
                display_lines: outcome_line_count(&outcome),
                outcome,
                elapsed_ms: 1,
                raw: None,
                cursor: None,
            }
        })
        .collect();

    prune_transcript_entries(&mut entries);

    assert!(transcript_line_count(&entries) <= MAX_TRANSCRIPT_LINES);
    assert_eq!(entries.last().map(|entry| entry.id), Some(2));
}

#[test]
fn command_history_prunes_from_front_with_incremental_byte_count() {
    let mut history = VecDeque::new();
    let mut total_bytes = 0;

    push_command_history(&mut history, &mut total_bytes, "GET a", 2, 11);
    push_command_history(&mut history, &mut total_bytes, "GET b", 2, 11);
    push_command_history(&mut history, &mut total_bytes, "GET c", 2, 11);

    assert_eq!(history.into_iter().collect::<Vec<_>>(), ["GET b", "GET c"]);
    assert_eq!(total_bytes, 10);
}

#[test]
fn command_history_skips_adjacent_duplicates() {
    let mut history = VecDeque::new();
    let mut total_bytes = 0;

    push_command_history(&mut history, &mut total_bytes, "PING", 10, 100);
    push_command_history(&mut history, &mut total_bytes, "PING", 10, 100);

    assert_eq!(history.len(), 1);
    assert_eq!(total_bytes, 4);
}

#[test]
fn pending_command_count_only_counts_in_flight_entries() {
    let entries = vec![
        Entry {
            id: 1,
            command: "PING".into(),
            db: 0,
            outcome: Outcome::Pending,
            display_lines: 1,
            elapsed_ms: 0,
            raw: None,
            cursor: None,
        },
        Entry {
            id: 2,
            command: "GET a".into(),
            db: 0,
            outcome: ok_lines("x"),
            display_lines: 1,
            elapsed_ms: 1,
            raw: None,
            cursor: None,
        },
    ];

    assert_eq!(pending_command_count(&entries), 1);
}

#[test]
fn clearing_transcript_preserves_in_flight_entries() {
    let mut entries = vec![
        Entry {
            id: 1,
            command: "BLPOP queue 10".into(),
            db: 0,
            outcome: Outcome::Pending,
            display_lines: 1,
            elapsed_ms: 0,
            raw: None,
            cursor: None,
        },
        Entry {
            id: 2,
            command: "PING".into(),
            db: 0,
            outcome: ok_lines("PONG"),
            display_lines: 1,
            elapsed_ms: 1,
            raw: None,
            cursor: None,
        },
    ];

    clear_completed_entries(&mut entries);

    assert_eq!(entries.len(), 1);
    assert!(matches!(entries[0].outcome, Outcome::Pending));
}

#[test]
fn command_preview_is_unicode_safe_and_visible() {
    assert_eq!(command_preview("你好世界", 2), "你好…（共 12 bytes）");
    assert_eq!(command_preview("PING", 10), "PING");
}

#[test]
fn retained_bytes_counts_nested_raw_payloads() {
    let value = ramag_domain::entities::RedisValue::Hash(vec![(
        "field".into(),
        ramag_domain::entities::RedisValue::Array(vec![
            ramag_domain::entities::RedisValue::Text("value".into()),
            ramag_domain::entities::RedisValue::Bytes(vec![0; 3]),
        ]),
    )]);

    assert_eq!(redis_value_retained_bytes(&value), 13);
}
