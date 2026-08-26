use ramag_domain::entities::Warning;
use ramag_ui::ResultMemoryUpdate;

use super::ResultPanel;

pub(crate) fn global_memory_warning(outcome: ResultMemoryUpdate) -> Warning {
    let total_mib = outcome.total_bytes / 1024 / 1024;
    let message = if outcome.evicted_results > 0 {
        format!(
            "全部查询标签结果达到全局预算，已按 LRU 释放 {} 个非活动标签的旧结果；当前保留约 {total_mib} MiB",
            outcome.evicted_results
        )
    } else {
        format!(
            "全部查询标签结果已达到 384 MiB 提示线（当前约 {total_mib} MiB），建议关闭旧结果或收窄查询"
        )
    };
    Warning {
        level: "Client".into(),
        code: 0,
        message,
    }
}

impl Drop for ResultPanel {
    fn drop(&mut self) {
        self.cancel_id_conversion();
        self.cancel_display_view_build();
    }
}
