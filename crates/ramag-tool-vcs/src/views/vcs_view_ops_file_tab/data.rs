//! 文件标签缓存与未跟踪 diff 构造。

use ramag_domain::entities::{DiffLine, DiffLineKind, FileChangeKind, FileDiff, Hunk};

use super::super::helpers::{FileContentSnapshot, FileTab};
use super::super::vcs_view_ops_repo::RawFileContent;

pub(super) fn prune_file_tab_payloads_to_budget(
    tabs: &mut [FileTab],
    active: Option<usize>,
    budget: usize,
) {
    // 活动标签和未保存草稿不可淘汰。
    let mut retained = tabs
        .iter()
        .enumerate()
        .filter(|(index, tab)| {
            Some(*index) == active
                || tab
                    .cached_content
                    .as_ref()
                    .is_some_and(|content| content.dirty)
        })
        .fold(0usize, |total, (_, tab)| {
            total.saturating_add(file_tab_payload_bytes(tab))
        });

    // 从新到旧保留非活动缓存。
    for index in (0..tabs.len()).rev() {
        if Some(index) == active
            || tabs[index]
                .cached_content
                .as_ref()
                .is_some_and(|content| content.dirty)
        {
            continue;
        }
        let bytes = file_tab_payload_bytes(&tabs[index]);
        if bytes == 0 {
            continue;
        }
        let Some(next) = retained.checked_add(bytes) else {
            clear_file_tab_payload(&mut tabs[index]);
            continue;
        };
        if next > budget {
            clear_file_tab_payload(&mut tabs[index]);
        } else {
            retained = next;
        }
    }
}

fn clear_file_tab_payload(tab: &mut FileTab) {
    tab.cached_diff = None;
    tab.cached_diff_syntax = None;
    tab.cached_content = None;
}

pub(super) fn file_tab_payload_bytes(tab: &FileTab) -> usize {
    tab.cached_diff
        .as_deref()
        .map_or(0, file_diff_payload_bytes)
        .saturating_add(
            tab.cached_diff_syntax
                .as_deref()
                .map_or(0, super::super::syntax::DiffSyntaxSnapshot::retained_bytes),
        )
        .saturating_add(
            tab.cached_content
                .as_ref()
                .map_or(0, file_content_payload_bytes),
        )
}

fn file_diff_payload_bytes(diff: &FileDiff) -> usize {
    let mut total = std::mem::size_of::<FileDiff>()
        .saturating_add(diff.path.capacity())
        .saturating_add(diff.old_path.as_ref().map_or(0, String::capacity))
        .saturating_add(
            diff.hunks
                .capacity()
                .saturating_mul(std::mem::size_of::<Hunk>()),
        );
    for hunk in &diff.hunks {
        total = total
            .saturating_add(hunk.heading.as_ref().map_or(0, String::capacity))
            .saturating_add(
                hunk.lines
                    .capacity()
                    .saturating_mul(std::mem::size_of::<DiffLine>()),
            );
        for line in &hunk.lines {
            total = total.saturating_add(line.text.capacity());
        }
    }
    total
}

fn file_content_payload_bytes(content: &FileContentSnapshot) -> usize {
    std::mem::size_of::<FileContentSnapshot>()
        .saturating_add(content.path.capacity())
        .saturating_add(content.text.capacity())
        .saturating_add(content.error.as_ref().map_or(0, String::capacity))
}

/// 将未跟踪文件转为全新增 diff，并保留二进制与截断标记。
pub(super) fn build_untracked_diff(raw: RawFileContent) -> FileDiff {
    let lines: Vec<DiffLine> = raw
        .lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| DiffLine {
            kind: DiffLineKind::Add,
            old_lineno: None,
            new_lineno: Some(i as u32 + 1),
            text,
        })
        .collect();
    let hunks = if lines.is_empty() {
        Vec::new()
    } else {
        vec![Hunk {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: lines.len() as u32,
            heading: raw
                .truncated
                .then(|| "文件过大，预览已截断（前 4MB）".to_string()),
            lines,
        }]
    };
    FileDiff {
        path: raw.path,
        old_path: None,
        change_kind: FileChangeKind::Untracked,
        binary: raw.binary,
        old_mode: None,
        new_mode: None,
        hunks,
    }
}
