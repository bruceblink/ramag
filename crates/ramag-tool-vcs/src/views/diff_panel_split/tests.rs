//! 拆分后的测试模块。

use super::*;
use ramag_domain::entities::{DiffLine, FileChangeKind, Hunk};

fn line(kind: DiffLineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        old_lineno: None,
        new_lineno: None,
        text: text.into(),
    }
}

fn sample_diff() -> Rc<FileDiff> {
    Rc::new(FileDiff {
        path: "a.rs".into(),
        old_path: None,
        change_kind: FileChangeKind::Modified,
        binary: false,
        old_mode: None,
        new_mode: None,
        hunks: vec![Hunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            heading: None,
            lines: vec![
                line(DiffLineKind::Delete, "old"),
                line(DiffLineKind::Add, "new value"),
            ],
        }],
    })
}

#[test]
#[ignore = "手动观察十万行 diff 布局与语法快照耗时"]
fn reports_large_diff_layout_latency() {
    use std::hint::black_box;
    use std::time::Instant;

    const LOGICAL_LINES: usize = 100_000;
    const ITERATIONS: usize = 5;

    let mut lines = Vec::with_capacity(LOGICAL_LINES + LOGICAL_LINES / 10);
    for index in 0..LOGICAL_LINES {
        if index % 10 == 0 {
            lines.push(line(
                DiffLineKind::Delete,
                &format!("let value_{index} = {index};"),
            ));
            lines.push(line(
                DiffLineKind::Add,
                &format!("let value_{index} = {};", index + 1),
            ));
        } else {
            lines.push(line(
                DiffLineKind::Context,
                &format!("let value_{index} = {index};"),
            ));
        }
    }
    let diff = Rc::new(FileDiff {
        path: "large.rs".into(),
        old_path: None,
        change_kind: FileChangeKind::Modified,
        binary: false,
        old_mode: None,
        new_mode: None,
        hunks: vec![Hunk {
            old_start: 1,
            old_lines: LOGICAL_LINES as u32,
            new_start: 1,
            new_lines: LOGICAL_LINES as u32,
            heading: None,
            lines,
        }],
    });
    let expanded = HashSet::new();

    let mut layout_samples = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let cache = RefCell::new(None);
        let started = Instant::now();
        black_box(prepare_diff_layout(&cache, &diff, false, true, &expanded));
        layout_samples.push(started.elapsed());
    }
    layout_samples.sort_unstable();

    let started = Instant::now();
    let syntax = black_box(super::super::syntax::DiffSyntaxSnapshot::new_bounded(
        &diff,
        Some("rust"),
    ));
    let syntax_elapsed = started.elapsed();
    eprintln!(
        "vcs large diff layout: domain_lines={}, layout_median={:.3} ms, syntax_gate={:.3} ms, syntax_built={}, syntax_bytes={}",
        diff.hunks[0].lines.len(),
        layout_samples[ITERATIONS / 2].as_secs_f64() * 1_000.0,
        syntax_elapsed.as_secs_f64() * 1_000.0,
        syntax.is_some(),
        syntax.as_ref().map_or(0, |syntax| syntax.retained_bytes())
    );
}

#[test]
#[allow(clippy::panic)]
fn layout_cache_reuses_rows_for_unchanged_diff_and_options() {
    let cache = RefCell::new(None);
    let diff = sample_diff();
    let expanded = HashSet::new();
    let first = prepare_diff_layout(&cache, &diff, false, true, &expanded);
    let second = prepare_diff_layout(&cache, &diff, false, true, &expanded);

    match (first, second) {
        (
            DiffLayout::Split {
                keys: first_keys,
                button_rows: first_buttons,
                ..
            },
            DiffLayout::Split {
                keys: second_keys,
                button_rows: second_buttons,
                ..
            },
        ) => {
            assert!(Rc::ptr_eq(&first_keys, &second_keys));
            assert!(Rc::ptr_eq(&first_buttons, &second_buttons));
        }
        _ => panic!("two-sided diff should use split layout"),
    }
}
