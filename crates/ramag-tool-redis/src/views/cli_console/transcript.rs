//! Redis CLI 记录保留、历史导航与内存预算。

use super::*;

pub(super) fn transcript_bytes(entries: &[Entry]) -> usize {
    entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(transcript_entry_bytes(entry))
    })
}

pub(super) fn prune_transcript_entries(entries: &mut Vec<Entry>) {
    let mut total_bytes = transcript_bytes(entries);
    let mut total_lines = transcript_line_count(entries);
    while entries.len() > MAX_TRANSCRIPT_ENTRIES
        || total_bytes > MAX_TRANSCRIPT_BYTES
        || total_lines > MAX_TRANSCRIPT_LINES
    {
        let Some(index) = entries
            .iter()
            .position(|entry| !matches!(entry.outcome, Outcome::Pending))
        else {
            break;
        };
        // 最新一条（当前结果）不因预算清除：单条超限时保留它、停止修剪
        if index + 1 == entries.len() {
            break;
        }
        let removed = entries.remove(index);
        total_bytes = total_bytes.saturating_sub(transcript_entry_bytes(&removed));
        total_lines = total_lines.saturating_sub(removed.display_lines);
    }
}

pub(super) fn transcript_entry_bytes(entry: &Entry) -> usize {
    let outcome_bytes = match &entry.outcome {
        Outcome::Pending => 0,
        Outcome::Ok(lines) => lines.iter().map(|line| line.len()).sum(),
        Outcome::Err(value) => value.len(),
    };
    let raw_bytes = entry
        .raw
        .as_deref()
        .map(redis_value_retained_bytes)
        .unwrap_or_default();
    entry
        .command
        .len()
        .saturating_add(outcome_bytes)
        .saturating_add(raw_bytes)
}

/// 估算续展开原始结果实际持有的载荷；容器开销不计也不会低估大值的主体数据。
pub(super) fn redis_value_retained_bytes(value: &RedisValue) -> usize {
    match value {
        RedisValue::Nil | RedisValue::Int(_) | RedisValue::Float(_) | RedisValue::Bool(_) => 0,
        RedisValue::Text(value) => value.len(),
        RedisValue::Bytes(value) => value.len(),
        RedisValue::List(values) | RedisValue::Set(values) | RedisValue::Array(values) => {
            values.iter().fold(0usize, |total, value| {
                total.saturating_add(redis_value_retained_bytes(value))
            })
        }
        RedisValue::Hash(pairs) => pairs.iter().fold(0usize, |total, (key, value)| {
            total
                .saturating_add(key.len())
                .saturating_add(redis_value_retained_bytes(value))
        }),
        RedisValue::ZSet(pairs) => pairs.iter().fold(0usize, |total, (member, _)| {
            total.saturating_add(redis_value_retained_bytes(member))
        }),
        RedisValue::Stream(entries) => entries.iter().fold(0usize, |total, entry| {
            entry.fields.iter().fold(
                total.saturating_add(entry.id.len()),
                |entry_total, (field, value)| {
                    entry_total
                        .saturating_add(field.len())
                        .saturating_add(value.len())
                },
            )
        }),
    }
}

pub(super) fn transcript_line_count(entries: &[Entry]) -> usize {
    entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(entry.display_lines)
    })
}

pub(super) fn outcome_line_count(outcome: &Outcome) -> usize {
    match outcome {
        Outcome::Pending => 1,
        Outcome::Ok(lines) => lines.len().max(1),
        Outcome::Err(value) => value.lines().count().max(1),
    }
}

pub(super) fn pending_command_count(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter(|entry| matches!(entry.outcome, Outcome::Pending))
        .count()
}

pub(super) fn clear_completed_entries(entries: &mut Vec<Entry>) {
    entries.retain(|entry| matches!(entry.outcome, Outcome::Pending));
}

pub(super) fn push_command_history(
    history: &mut VecDeque<String>,
    total_bytes: &mut usize,
    command: &str,
    max_entries: usize,
    max_bytes: usize,
) {
    if history.back().map(String::as_str) == Some(command) {
        return;
    }

    *total_bytes = total_bytes.saturating_add(command.len());
    history.push_back(command.to_string());
    while history.len() > max_entries || *total_bytes > max_bytes {
        let Some(removed) = history.pop_front() else {
            *total_bytes = 0;
            break;
        };
        *total_bytes = total_bytes.saturating_sub(removed.len());
    }
}

pub(super) fn command_preview(command: &str, max_chars: usize) -> String {
    let mut chars = command.chars();
    let mut preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        preview.push_str(&format!("…（共 {} bytes）", command.len()));
    }
    preview
}

pub(super) fn prev_cursor(len: usize, cur: Option<usize>) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match cur {
        None => len - 1,
        Some(0) => 0,
        Some(i) => i - 1,
    })
}

pub(super) fn next_cursor(len: usize, cur: usize) -> Option<usize> {
    if cur + 1 < len { Some(cur + 1) } else { None }
}
