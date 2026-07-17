//! DB Client 视图集合：DbClientView 根视图 + 连接 / 表单 / 表树等子面板

pub mod cell_edit_dialog;
pub mod connection_form;
pub mod connection_list;
pub mod connection_session;
pub mod dbclient_view;
pub mod ddl;
pub mod history_dialog;
pub mod query_panel;
pub mod query_tab;
pub mod result_panel;
pub mod result_table;
pub mod table_tree;
pub mod tree_helpers;

pub use dbclient_view::DbClientView;

pub(super) fn inline_text_preview(text: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(text.len().min(max_chars.saturating_mul(4)));
    let mut chars = text.chars();
    for character in chars.by_ref().take(max_chars) {
        if matches!(character, '\n' | '\r' | '\t') {
            output.push(' ');
        } else if character.is_control() {
            output.push('\u{fffd}');
        } else {
            output.push(character);
        }
    }
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod inline_text_tests {
    use super::inline_text_preview;

    #[test]
    fn inline_preview_is_single_line_and_unicode_safe() {
        assert_eq!(inline_text_preview("你好世界", 2), "你好…");
        assert_eq!(inline_text_preview("a\nb\0c", 20), "a b�c");
    }
}
