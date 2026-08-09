use super::{
    MAX_GIT_MESSAGE_BYTES, MAX_GIT_RECORD_BYTES, MAX_PARSED_GIT_ITEMS, PROGRESS_LINE_MAX_BYTES,
    command, encode_pathspecs, ensure_git_list_room, ensure_git_message_size,
    ensure_git_record_size, read_limited, record_progress_line, validate_name_arg,
    validate_path_arg, validate_path_args, validate_positional_arg,
};
use ramag_domain::entities::{
    MAX_GIT_NAME_ARG_BYTES, MAX_GIT_PATH_ARGS_BYTES, MAX_GIT_POSITIONAL_ARG_BYTES,
};

#[test]
fn git_terminal_prompt_is_disabled_for_gui_processes() {
    let command = command();
    let prompt = command
        .get_envs()
        .find(|(key, _)| *key == "GIT_TERMINAL_PROMPT")
        .and_then(|(_, value)| value);
    assert_eq!(prompt, Some(std::ffi::OsStr::new("0")));
    let editor = command
        .get_envs()
        .find(|(key, _)| *key == "GIT_EDITOR")
        .and_then(|(_, value)| value);
    assert_eq!(editor, Some(std::ffi::OsStr::new("true")));
    let literal_pathspecs = command
        .get_envs()
        .find(|(key, _)| *key == "GIT_LITERAL_PATHSPECS")
        .and_then(|(_, value)| value);
    assert_eq!(literal_pathspecs, Some(std::ffi::OsStr::new("1")));
}

#[test]
fn progress_lines_are_bounded_and_keep_truncation_hint() -> std::result::Result<(), String> {
    let mut line = vec![b'x'; PROGRESS_LINE_MAX_BYTES];
    let mut truncated = true;
    let progress = std::sync::Mutex::new(String::new());
    let mut tail = std::collections::VecDeque::new();

    record_progress_line(&mut line, &mut truncated, &progress, &mut tail);

    let text = progress
        .lock()
        .map_err(|_| "progress lock should not be poisoned".to_string())?;
    assert!(text.ends_with(" …"));
    assert!(text.len() <= PROGRESS_LINE_MAX_BYTES + " …".len());
    assert!(line.is_empty());
    assert!(!truncated);
    Ok(())
}

#[test]
fn captured_output_stops_after_the_safety_limit() -> std::io::Result<()> {
    let captured = read_limited(std::io::Cursor::new(b"123456"), 4)?;
    assert_eq!(captured.bytes, b"1234");
    assert!(captured.truncated);

    let oversized = vec![b'x'; 32 * 1024];
    let mut cursor = std::io::Cursor::new(&oversized);
    let captured = read_limited(&mut cursor, 4)?;
    assert_eq!(captured.bytes, b"xxxx");
    assert!(captured.truncated);
    assert_eq!(cursor.position(), oversized.len() as u64);

    let exact = read_limited(std::io::Cursor::new(b"1234"), 4)?;
    assert_eq!(exact.bytes, b"1234");
    assert!(!exact.truncated);
    Ok(())
}

#[test]
fn user_arguments_cannot_be_interpreted_as_options() {
    assert!(validate_name_arg("feature/test", "分支名").is_ok());
    assert!(validate_name_arg("--delete", "分支名").is_err());
    assert!(validate_name_arg("bad name", "分支名").is_err());
    assert!(validate_positional_arg("HEAD~2", "revision").is_ok());
    assert!(validate_positional_arg("--exec=bad", "revision").is_err());
    assert!(validate_positional_arg("bad\nvalue", "revision").is_err());
    assert!(validate_name_arg(&"n".repeat(MAX_GIT_NAME_ARG_BYTES), "分支名").is_ok());
    assert!(validate_name_arg(&"n".repeat(MAX_GIT_NAME_ARG_BYTES + 1), "分支名").is_err());
    assert!(validate_positional_arg(&"u".repeat(MAX_GIT_POSITIONAL_ARG_BYTES), "远程 URL").is_ok());
    assert!(
        validate_positional_arg(&"u".repeat(MAX_GIT_POSITIONAL_ARG_BYTES + 1), "远程 URL").is_err()
    );
}

#[test]
fn pathspec_input_preserves_special_names_and_has_batch_boundaries()
-> ramag_domain::error::Result<()> {
    let paths = vec![
        "-leading".to_string(),
        ":(glob)*".to_string(),
        "a\nb".to_string(),
    ];
    let encoded = encode_pathspecs(&paths)?;
    assert_eq!(encoded.as_bytes(), b"-leading\0:(glob)*\0a\nb\0");
    assert!(validate_path_arg("a\nb", "路径").is_ok());
    assert!(validate_path_arg("bad\0path", "路径").is_err());
    assert!(
        validate_path_arg(
            &"d/".repeat(ramag_domain::entities::MAX_GIT_PATH_DEPTH),
            "路径"
        )
        .is_err()
    );

    let oversized = vec!["x".repeat(MAX_GIT_PATH_ARGS_BYTES)];
    assert!(validate_path_args(&oversized, "路径列表").is_err());
    Ok(())
}

#[test]
fn parsed_git_entity_budgets_enforce_boundaries() {
    assert!(ensure_git_list_room(MAX_PARSED_GIT_ITEMS - 1, "列表").is_ok());
    assert!(ensure_git_list_room(MAX_PARSED_GIT_ITEMS, "列表").is_err());
    assert!(ensure_git_record_size(&vec![0; MAX_GIT_RECORD_BYTES], "记录", 1).is_ok());
    assert!(ensure_git_record_size(&vec![0; MAX_GIT_RECORD_BYTES + 1], "记录", 1).is_err());
    assert!(ensure_git_message_size(&vec![0; MAX_GIT_MESSAGE_BYTES], "正文", 1).is_ok());
    assert!(ensure_git_message_size(&vec![0; MAX_GIT_MESSAGE_BYTES + 1], "正文", 1).is_err());
}
