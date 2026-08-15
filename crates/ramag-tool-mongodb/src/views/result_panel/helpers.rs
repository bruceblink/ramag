use ramag_domain::entities::{
    INTERACTIVE_RESULT_WARNING_BYTES, MongoQueryResult, json_pretty_bounded,
};
use ramag_ui::ResultMemoryUpdate;

pub(super) fn bounded_cell_dialog_text(mut text: String, max_bytes: usize) -> String {
    const TRUNCATED_NOTICE: &str = "\n\n[内容过大，仅显示开头部分]";
    if text.len() <= max_bytes {
        return text;
    }

    let mut end = max_bytes.saturating_sub(TRUNCATED_NOTICE.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(TRUNCATED_NOTICE);
    text
}

pub(super) fn memory_notice(
    result: &MongoQueryResult,
    retained_bytes: usize,
    outcome: ResultMemoryUpdate,
) -> Option<String> {
    let mut notices = Vec::new();
    if result.memory_warning || retained_bytes >= INTERACTIVE_RESULT_WARNING_BYTES {
        notices.push(
            "单个结果及表格视图已达到 128 MiB 提示线，建议用 filter / projection 收窄查询"
                .to_string(),
        );
    }
    if outcome.warning {
        if outcome.evicted_results > 0 {
            notices.push(format!(
                "全部查询标签结果达到全局预算，已按 LRU 释放 {} 个非活动标签的旧结果",
                outcome.evicted_results
            ));
        } else {
            notices.push(format!(
                "全部查询标签结果已达到 384 MiB 提示线（当前约 {} MiB）",
                outcome.total_bytes / 1024 / 1024
            ));
        }
    }
    (!notices.is_empty()).then(|| notices.join("；"))
}

pub(super) fn pretty_cell_value(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes && (text.starts_with('{') || text.starts_with('[')) {
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| json_pretty_bounded(&value, max_bytes))
            .unwrap_or(text)
    } else {
        text
    }
}
