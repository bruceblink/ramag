//! 进程表的固定列尺寸策略。

/// 进程表在窄窗口收紧固定列，为进程名保留剩余空间且不丢失关键监控字段。
#[derive(Clone, Copy)]
pub(crate) struct ProcessTableLayout {
    pub(crate) pid_width: f32,
    pub(crate) process_name_min_width: f32,
    pub(crate) cpu_width: f32,
    pub(crate) memory_width: f32,
    pub(crate) action_width: f32,
    pub(crate) gap: f32,
    pub(crate) horizontal_padding: f32,
}

/// 返回当前窗口密度对应的进程表列宽，常规窗口继续使用完整的桌面尺寸。
pub(crate) fn process_table_layout(compact: bool) -> ProcessTableLayout {
    if compact {
        ProcessTableLayout {
            pid_width: 48.0,
            process_name_min_width: 0.0,
            cpu_width: 58.0,
            memory_width: 84.0,
            action_width: 28.0,
            gap: 4.0,
            horizontal_padding: 8.0,
        }
    } else {
        ProcessTableLayout {
            pid_width: 70.0,
            process_name_min_width: 140.0,
            cpu_width: 84.0,
            memory_width: 110.0,
            action_width: 42.0,
            gap: 8.0,
            horizontal_padding: 12.0,
        }
    }
}
