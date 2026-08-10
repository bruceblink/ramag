pub mod collection_tree;
pub mod examples;
pub mod history_dialog;
pub mod mongo_session;
pub mod query_panel;
pub mod query_tab;
pub mod result_panel;

pub use mongo_session::MongoSessionPanel;

pub(crate) const MAX_MONGO_INTERACTIVE_INPUT_BYTES: usize = ramag_ui::MAX_EDITOR_DRAFT_BYTES;
const JSON_VALUE_NODE_OVERHEAD_BYTES: usize = 64;

pub(crate) fn bounded_input(
    window: &mut gpui::Window,
    cx: &mut gpui::Context<gpui_component::input::InputState>,
) -> gpui_component::input::InputState {
    gpui_component::input::InputState::new(window, cx)
        .validate(|value, _| value.len() <= MAX_MONGO_INTERACTIVE_INPUT_BYTES)
}

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

pub(crate) fn reserve_input_bytes(current: usize, added: usize) -> Option<usize> {
    current
        .checked_add(added)
        .filter(|total| *total <= MAX_MONGO_INTERACTIVE_INPUT_BYTES)
}

pub(crate) fn estimated_json_value_bytes(value: &serde_json::Value) -> usize {
    let mut total = 0usize;
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        total = total.saturating_add(JSON_VALUE_NODE_OVERHEAD_BYTES);
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    total = total.saturating_add(key.len());
                    stack.push(child);
                }
            }
            serde_json::Value::Array(items) => stack.extend(items),
            serde_json::Value::String(text) => total = total.saturating_add(text.len()),
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        JSON_VALUE_NODE_OVERHEAD_BYTES, MAX_MONGO_INTERACTIVE_INPUT_BYTES,
        estimated_json_value_bytes, inline_text_preview, reserve_input_bytes,
    };

    #[test]
    fn inline_preview_is_single_line_and_unicode_safe() {
        assert_eq!(inline_text_preview("你好世界", 2), "你好…");
        assert_eq!(inline_text_preview("a\nb\0c", 20), "a b�c");
    }

    #[test]
    fn interactive_input_budget_has_an_exact_boundary() {
        assert_eq!(
            reserve_input_bytes(MAX_MONGO_INTERACTIVE_INPUT_BYTES - 1, 1),
            Some(MAX_MONGO_INTERACTIVE_INPUT_BYTES)
        );
        assert_eq!(
            reserve_input_bytes(MAX_MONGO_INTERACTIVE_INPUT_BYTES, 1),
            None
        );
    }

    #[test]
    fn json_value_estimate_counts_nodes_keys_and_strings() {
        let value = json!({"name": "alice", "items": [1, 2]});
        let bytes = estimated_json_value_bytes(&value);

        assert!(bytes >= 5 * JSON_VALUE_NODE_OVERHEAD_BYTES);
        assert!(bytes >= "nameitemsalice".len());
    }
}
