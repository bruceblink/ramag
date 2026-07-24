//! SQL 结果表双轴滚动事件接入。

use gpui::{Context, ScrollWheelEvent, Window};

use super::ResultPanel;

impl ResultPanel {
    pub(in crate::views) fn on_result_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let horizontal = self.h_scroll.clone();
        let vertical = self.uniform_scroll.0.borrow().base_handle.clone();
        ramag_ui::handle_axis_scroll(
            &mut self.result_scroll_gesture,
            event,
            window,
            &horizontal,
            &vertical,
            cx,
        );
    }
}
