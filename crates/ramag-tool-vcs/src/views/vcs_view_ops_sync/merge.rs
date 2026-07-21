use std::collections::HashSet;

use ramag_domain::entities::{FileStatus, WorkingTreeStatus};

pub(super) fn status_changes(old: &WorkingTreeStatus, new: &WorkingTreeStatus) -> (bool, bool) {
    (
        old.files != new.files,
        old.head_commit != new.head_commit || old.head_branch != new.head_branch,
    )
}

/// 用路径级 status 替换受影响的文件，未命中的旧状态原样保留。
pub(super) fn merge_partial_status(
    status: &mut WorkingTreeStatus,
    paths: &[String],
    mut incoming: Vec<FileStatus>,
) -> bool {
    let prefixes: HashSet<&str> = paths.iter().map(String::as_str).collect();
    incoming.sort_unstable_by(compare_file_status);
    let incoming_identities: HashSet<&str> = incoming
        .iter()
        .flat_map(|file| std::iter::once(file.path.as_str()).chain(file.old_path.as_deref()))
        .collect();

    let mut ranges = affected_status_ranges(&status.files, paths);
    for identity in &incoming_identities {
        push_exact_status_range(&status.files, identity, &mut ranges);
    }
    for (index, file) in status.files.iter().enumerate() {
        if file.old_path.as_deref().is_some_and(|old_path| {
            path_matches_prefixes(old_path, &prefixes) || incoming_identities.contains(old_path)
        }) {
            ranges.push(index..index + 1);
        }
    }
    drop(incoming_identities);
    merge_ranges(&mut ranges);

    let existing_count = ranges.iter().map(|range| range.len()).sum::<usize>();
    if existing_count == incoming.len()
        && ranges
            .iter()
            .flat_map(|range| status.files[range.clone()].iter())
            .eq(incoming.iter())
    {
        return false;
    }

    let mut unaffected = Vec::with_capacity(status.files.len().saturating_sub(existing_count));
    let mut range_index = 0usize;
    for (index, file) in std::mem::take(&mut status.files).into_iter().enumerate() {
        while ranges
            .get(range_index)
            .is_some_and(|range| index >= range.end)
        {
            range_index += 1;
        }
        if ranges
            .get(range_index)
            .is_some_and(|range| range.contains(&index))
        {
            continue;
        }
        unaffected.push(file);
    }
    status.files = merge_sorted_statuses(unaffected, incoming);
    true
}

fn affected_status_ranges(files: &[FileStatus], paths: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::with_capacity(paths.len().saturating_mul(2));
    for prefix in paths {
        push_exact_status_range(files, prefix, &mut ranges);
        let descendant_prefix = format!("{prefix}/");
        let start = files.partition_point(|file| file.path < descendant_prefix);
        let count =
            files[start..].partition_point(|file| file.path.starts_with(&descendant_prefix));
        if count > 0 {
            ranges.push(start..start + count);
        }
    }
    ranges
}

fn push_exact_status_range(
    files: &[FileStatus],
    path: &str,
    ranges: &mut Vec<std::ops::Range<usize>>,
) {
    let start = files.partition_point(|file| file.path.as_str() < path);
    let count = files[start..].partition_point(|file| file.path == path);
    if count > 0 {
        ranges.push(start..start + count);
    }
}

fn merge_ranges(ranges: &mut Vec<std::ops::Range<usize>>) {
    ranges.sort_unstable_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    *ranges = merged;
}

pub(super) fn path_matches_prefixes(path: &str, prefixes: &HashSet<&str>) -> bool {
    if prefixes.contains(path) {
        return true;
    }
    let mut candidate = path;
    while let Some((parent, _)) = candidate.rsplit_once('/') {
        if prefixes.contains(parent) {
            return true;
        }
        candidate = parent;
    }
    false
}

/// 用路径级 `ls-files` 结果替换 Project Files 中受影响成员；无成员变化时保持原 Vec 身份。
pub(super) fn merge_partial_project_files(
    project_files: &mut Vec<String>,
    paths: &[String],
    mut incoming: Vec<String>,
) -> bool {
    let prefixes = paths.iter().map(String::as_str).collect::<HashSet<_>>();
    incoming.retain(|path| path_matches_prefixes(path, &prefixes));
    incoming.sort_unstable();
    incoming.dedup();

    let ranges = affected_project_ranges(project_files, paths);
    let existing = ranges
        .iter()
        .flat_map(|range| project_files[range.clone()].iter().map(String::as_str))
        .collect::<Vec<_>>();
    if existing.len() == incoming.len()
        && existing
            .iter()
            .zip(&incoming)
            .all(|(left, right)| *left == right)
    {
        return false;
    }

    let affected_count = ranges.iter().map(|range| range.len()).sum::<usize>();
    let mut unaffected = Vec::with_capacity(project_files.len().saturating_sub(affected_count));
    let mut range_index = 0usize;
    for (index, path) in std::mem::take(project_files).into_iter().enumerate() {
        while ranges
            .get(range_index)
            .is_some_and(|range| index >= range.end)
        {
            range_index += 1;
        }
        if ranges
            .get(range_index)
            .is_some_and(|range| range.contains(&index))
        {
            continue;
        }
        unaffected.push(path);
    }
    *project_files = merge_sorted_paths(unaffected, incoming);
    true
}

fn affected_project_ranges(
    project_files: &[String],
    paths: &[String],
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::with_capacity(paths.len().saturating_mul(2));
    for prefix in paths {
        if let Ok(index) = project_files.binary_search(prefix) {
            ranges.push(index..index + 1);
        }
        let descendant_prefix = format!("{prefix}/");
        let start = project_files.partition_point(|path| path < &descendant_prefix);
        let count =
            project_files[start..].partition_point(|path| path.starts_with(&descendant_prefix));
        if count > 0 {
            ranges.push(start..start + count);
        }
    }
    ranges.sort_unstable_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn merge_sorted_paths(left: Vec<String>, right: Vec<String>) -> Vec<String> {
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    let mut merged = Vec::with_capacity(left.len().saturating_add(right.len()));
    loop {
        match (left.peek(), right.peek()) {
            (Some(left_path), Some(right_path)) => match left_path.cmp(right_path) {
                std::cmp::Ordering::Less => {
                    if let Some(path) = left.next() {
                        merged.push(path);
                    }
                }
                std::cmp::Ordering::Equal => {
                    if let Some(path) = left.next() {
                        merged.push(path);
                    }
                    let _ = right.next();
                }
                std::cmp::Ordering::Greater => {
                    if let Some(path) = right.next() {
                        merged.push(path);
                    }
                }
            },
            (Some(_), None) => {
                merged.extend(left);
                break;
            }
            (None, Some(_)) => {
                merged.extend(right);
                break;
            }
            (None, None) => break,
        }
    }
    merged
}

fn merge_sorted_statuses(left: Vec<FileStatus>, right: Vec<FileStatus>) -> Vec<FileStatus> {
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    let mut merged = Vec::with_capacity(left.len().saturating_add(right.len()));
    loop {
        match (left.peek(), right.peek()) {
            (Some(left_file), Some(right_file)) => {
                match compare_file_status(left_file, right_file) {
                    std::cmp::Ordering::Less => {
                        if let Some(file) = left.next() {
                            merged.push(file);
                        }
                    }
                    std::cmp::Ordering::Equal => {
                        if let Some(file) = right.next() {
                            merged.push(file);
                        }
                        let _ = left.next();
                    }
                    std::cmp::Ordering::Greater => {
                        if let Some(file) = right.next() {
                            merged.push(file);
                        }
                    }
                }
            }
            (Some(_), None) => {
                merged.extend(left);
                break;
            }
            (None, Some(_)) => {
                merged.extend(right);
                break;
            }
            (None, None) => break,
        }
    }
    merged
}

fn compare_file_status(left: &FileStatus, right: &FileStatus) -> std::cmp::Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.old_path.cmp(&right.old_path))
}
