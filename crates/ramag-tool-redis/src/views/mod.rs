//! Redis 视图：dbclient 装载，driver 选 Redis 时进入 connection_session

pub mod cli_console;
pub mod connection_session;
pub mod form_shell;
pub mod hash_field_form;
pub mod key_create;
pub mod key_detail;
pub mod key_tree;
pub mod lines_editor;
pub mod list_element_form;
pub mod pairs_editor;
pub mod set_element_form;
pub mod stream_entry_form;
pub mod ttl_edit;
pub mod ttl_picker;
pub mod value_display;
pub mod value_edit;
pub mod zset_element_form;

pub use connection_session::RedisSessionPanel;

pub(crate) fn bounded_input(
    max_bytes: usize,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<gpui_component::input::InputState>,
) -> gpui_component::input::InputState {
    gpui_component::input::InputState::new(window, cx)
        .validate(move |value, _| value.len() <= max_bytes)
}

/// 单行标签只生成有限预览，并清理 GPUI 单行 shaping 不接受的控制字符。
pub(crate) fn inline_text_preview(text: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(text.len().min(max_chars.saturating_mul(4)));
    let mut chars = text.chars();
    for ch in chars.by_ref().take(max_chars) {
        if ch == '\n' || ch == '\r' || ch == '\t' {
            output.push(' ');
        } else if ch.is_control() {
            output.push('\u{fffd}');
        } else {
            output.push(ch);
        }
    }
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

pub(crate) fn reserve_command_input_bytes(current: usize, added: usize) -> Option<usize> {
    current
        .checked_add(added)
        .filter(|total| *total <= ramag_domain::entities::MAX_REDIS_COMMAND_BYTES)
}

#[cfg(test)]
mod tests {
    use super::{inline_text_preview, reserve_command_input_bytes};
    use ramag_domain::entities::MAX_REDIS_COMMAND_BYTES;

    #[test]
    fn inline_preview_is_unicode_safe_bounded_and_single_line() {
        assert_eq!(inline_text_preview("你好世界", 2), "你好…");
        assert_eq!(inline_text_preview("a\nb\tc\0d", 20), "a b c�d");
        assert_eq!(inline_text_preview("short", 20), "short");
    }

    #[test]
    fn command_input_budget_accepts_boundary_and_rejects_overflow() {
        assert_eq!(
            reserve_command_input_bytes(MAX_REDIS_COMMAND_BYTES - 1, 1),
            Some(MAX_REDIS_COMMAND_BYTES)
        );
        assert_eq!(
            reserve_command_input_bytes(MAX_REDIS_COMMAND_BYTES, 1),
            None
        );
        assert_eq!(reserve_command_input_bytes(usize::MAX, 1), None);
    }
}
