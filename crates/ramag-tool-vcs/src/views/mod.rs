pub mod branch_picker;
pub mod commit_detail;
pub mod commit_graph;
pub mod compare_panel;
pub mod confirm_dialogs;
pub mod conflict_editor;
pub mod diff_keys;
pub mod diff_panel;
pub mod diff_panel_split;
pub mod diff_split_cells;
pub mod file_tree;
pub mod helpers;
pub mod history_panel;
mod history_retention;
pub mod ide_layout;
mod latest_write;
pub mod pf_content;
pub mod project_files;
pub mod rebase_plan;
pub mod reflog_view;
pub mod repo_list;
pub mod sidebar;
pub mod sidebar_branches;
pub mod sidebar_remotes;
pub mod sidebar_stash;
pub mod sidebar_tags;
pub mod syntax;
pub mod vcs_tabs;
pub mod vcs_toolbar;
pub mod vcs_view;
pub mod vcs_view_ops;
pub mod vcs_view_ops_compare;
pub mod vcs_view_ops_file_tab;
pub mod vcs_view_ops_history;
pub mod vcs_view_ops_merge;
pub mod vcs_view_ops_patch;
pub mod vcs_view_ops_remote;
pub mod vcs_view_ops_repo;
pub mod vcs_view_ops_sync;
pub mod workspace_commit;
pub mod workspace_conflict;
pub mod workspace_diff;
pub mod workspace_panel;

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
