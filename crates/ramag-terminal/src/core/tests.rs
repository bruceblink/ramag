use super::*;
use alacritty_terminal::vte::ansi::Processor;

fn parsed_snapshot(input: &[u8], columns: usize, lines: usize, history: usize) -> TerminalSnapshot {
    let dimensions = TerminalDimensions::new(columns, lines);
    let shared = Arc::new(SharedState::new(dimensions.window_size(8, 18)));
    let proxy = TerminalEventProxy { shared };
    let mut term = Term::new(terminal_config(history), &dimensions, proxy);
    let mut processor: Processor = Processor::default();
    processor.advance(&mut term, input);
    snapshot_term(&term)
}

fn row_text(snapshot: &TerminalSnapshot, row: usize) -> String {
    snapshot.rows[row]
        .iter()
        .filter(|cell| !cell.wide_spacer)
        .map(|cell| cell.text.as_str())
        .collect::<String>()
}

#[test]
fn ansi_colors_and_unicode_are_preserved() {
    let snapshot = parsed_snapshot("\u{1b}[31m红色\u{1b}[0m".as_bytes(), 10, 2, 10);
    assert!(row_text(&snapshot, 0).starts_with("红色"));
    assert_ne!(snapshot.rows[0][0].foreground, default_foreground());
    assert!(snapshot.rows[0][1].wide_spacer);
}

#[test]
fn alternate_screen_is_independent() {
    let snapshot = parsed_snapshot(b"main\x1b[?1049h\rALT", 10, 2, 10);
    assert!(snapshot.alternate_screen);
    assert!(row_text(&snapshot, 0).starts_with("ALT"));

    let snapshot = parsed_snapshot(b"main\x1b[?1049h\rALT\x1b[?1049l", 10, 2, 10);
    assert!(!snapshot.alternate_screen);
    assert!(row_text(&snapshot, 0).starts_with("main"));
}

#[test]
fn cursor_position_shape_and_input_modes_are_exposed() {
    let snapshot = parsed_snapshot(b"\x1b[2;3H\x1b[?1h\x1b[?2004h\x1b[6 q", 10, 4, 10);
    assert_eq!(
        snapshot.cursor,
        Some(TerminalCursor {
            row: 1,
            column: 2,
            shape: TerminalCursorShape::Beam,
        })
    );
    assert!(snapshot.application_cursor);
    assert!(snapshot.bracketed_paste);
}

#[test]
fn history_is_bounded() {
    let mut input = Vec::new();
    for index in 0..100 {
        input.extend_from_slice(format!("{index}\r\n").as_bytes());
    }
    let dimensions = TerminalDimensions::new(10, 2);
    let shared = Arc::new(SharedState::new(dimensions.window_size(8, 18)));
    let proxy = TerminalEventProxy { shared };
    let mut term = Term::new(terminal_config(5), &dimensions, proxy);
    let mut processor: Processor = Processor::default();
    processor.advance(&mut term, &input);
    assert!(term.grid().history_size() <= 5);
}

#[test]
fn command_rejects_nul_without_spawning() {
    let command = TerminalCommand::new("ssh", vec!["bad\0argument".into()]);
    assert!(command.validate().is_err());
}

#[test]
fn paste_is_bounded_by_one_bracket_pair_and_strips_nul() {
    assert_eq!(encode_paste("a\0b", false), b"ab");
    assert_eq!(
        encode_paste("a\u{1b}[201~b\u{1b}[200~c", true),
        b"\x1b[200~abc\x1b[201~"
    );
}

#[test]
fn terminal_resource_and_clipboard_policies_are_explicit() {
    let config = terminal_config(123);
    assert_eq!(config.scrolling_history, 123);
    assert_eq!(config.osc52, Osc52::Disabled);

    let dimensions = TerminalDimensions::new(usize::MAX, 0);
    assert_eq!(dimensions.columns(), u16::MAX as usize);
    assert_eq!(dimensions.screen_lines(), 1);
}

#[cfg(unix)]
#[test]
fn local_pty_drains_output_and_reports_child_exit() {
    let mut core = TerminalCore::start(TerminalCommand::new(
        "/bin/echo",
        vec!["ramag-pty-ok".into()],
    ))
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while core.exit_status().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(core.exit_status().is_some_and(|status| status.success));
    assert!(row_text(&core.snapshot(), 0).contains("ramag-pty-ok"));
    core.close();
    assert!(core.is_closed());
}

#[cfg(unix)]
#[test]
fn local_pty_resize_updates_grid_dimensions() {
    let mut core = TerminalCore::start(TerminalCommand::new("/bin/cat", Vec::new())).unwrap();
    core.resize(120, 40, 9, 20).unwrap();

    let snapshot = core.snapshot();
    assert_eq!(snapshot.columns, 120);
    assert_eq!(snapshot.lines, 40);
    core.close();
}
