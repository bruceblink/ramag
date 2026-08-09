use super::{MainWindowOpenGate, build_tool_registry};

#[test]
fn main_window_open_gate_coalesces_repeated_requests() {
    let mut gate = MainWindowOpenGate::default();

    assert!(gate.try_begin());
    assert!(!gate.try_begin());
    gate.finish();
    assert!(gate.try_begin());
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn clipboard_capture_retry_backoff_is_bounded() {
    use super::{CAPTURE_INTERVAL, CAPTURE_MAX_RETRY_INTERVAL, next_capture_retry_interval};

    let mut interval = CAPTURE_INTERVAL;
    assert_eq!(
        next_capture_retry_interval(interval),
        CAPTURE_INTERVAL.saturating_mul(2)
    );
    for _ in 0..16 {
        interval = next_capture_retry_interval(interval);
    }
    assert_eq!(interval, CAPTURE_MAX_RETRY_INTERVAL);
    assert_eq!(
        next_capture_retry_interval(interval),
        CAPTURE_MAX_RETRY_INTERVAL
    );
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn clipboard_tool_is_registered_last() {
    let ids = build_tool_registry()
        .list()
        .into_iter()
        .map(|tool| tool.meta().id.clone())
        .collect::<Vec<_>>();

    assert_eq!(ids, ["dbclient", "vcs", "ssh", "clipboard"]);
}

#[cfg(target_os = "linux")]
#[test]
fn clipboard_tool_is_not_registered_on_linux() {
    let ids = build_tool_registry()
        .list()
        .into_iter()
        .map(|tool| tool.meta().id.clone())
        .collect::<Vec<_>>();

    assert_eq!(ids, ["dbclient", "vcs", "ssh"]);
}
