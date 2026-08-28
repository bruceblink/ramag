//! 结果表单元格输入框的生命周期和只读原因。

use gpui::{AppContext as _, Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};
use ramag_domain::entities::{MAX_SQL_QUERY_BYTES, QueryResult};

use super::{ResultPanel, ResultState};

impl ResultPanel {
    /// 在结果表中创建一个单行编辑器，回车暂存，失焦只取消不写库。
    pub(in crate::views) fn begin_cell_edit(
        &mut self,
        ri: usize,
        ci: usize,
        initial_text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_cell_edit_state();
        let input =
            cx.new(|cx_inner| InputState::new(window, cx_inner).default_value(initial_text));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => this.commit_cell_edit(cx),
                InputEvent::Blur => this.cancel_inline_cell_edit(cx),
                InputEvent::Change | InputEvent::Focus => {}
            },
        );
        self.editing_cell = Some((ri, ci));
        self.cell_edit_input = Some(input.clone());
        self.cell_edit_subscription = Some(subscription);
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// 取消当前单元格编辑并释放输入控件，避免旧结果继续持有编辑器。
    pub(in crate::views) fn cancel_inline_cell_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing_cell.is_none() {
            return;
        }
        self.clear_cell_edit_state();
        cx.notify();
    }

    /// 读取输入框文本并复用现有 UPDATE 安全链；校验失败时保留编辑器供修正。
    fn commit_cell_edit(&mut self, cx: &mut Context<Self>) {
        let Some((ri, ci)) = self.editing_cell else {
            return;
        };
        let Some(input) = self.cell_edit_input.clone() else {
            return;
        };
        let new_text = input.read(cx).value().to_string();
        if self.stage_cell_update(ri, ci, new_text, cx) {
            self.clear_cell_edit_state();
            cx.notify();
        }
    }

    pub(super) fn clear_cell_edit_state(&mut self) {
        self.cell_edit_subscription = None;
        self.cell_edit_input = None;
        self.editing_cell = None;
    }

    pub(in crate::views) fn set_cell_edit_input(&mut self, input: Option<Entity<InputState>>) {
        match input {
            Some(input) => self.cell_edit_input = Some(input),
            None => self.clear_cell_edit_state(),
        }
    }

    pub(in crate::views) fn cell_info(
        &self,
        ri: usize,
        ci: usize,
    ) -> Option<(String, String, bool)> {
        let ResultState::Ok(result) = &self.state else {
            return None;
        };
        let col_name = result.columns.get(ci)?.clone();
        let val = self.cell_value(ri, ci)?;
        let (display, truncated) = val.display_for_edit_bounded(MAX_SQL_QUERY_BYTES);
        Some((col_name, display, truncated))
    }

    pub(in crate::views) fn cell_edit_block_reason(&self, ri: usize, ci: usize) -> Option<String> {
        if let Some(reason) = self.modify_block_reason() {
            return Some(reason.to_string());
        }
        if self.cell_is_binary(ri, ci) {
            return Some(
                "二进制内容显示为 hex 文本，直接保存会损坏原始字节，仅可查看 / 复制".to_string(),
            );
        }
        None
    }

    pub(in crate::views) fn identity_label(&self) -> &'static str {
        self.row_identity
            .as_ref()
            .map(|i| i.label)
            .unwrap_or("主键")
    }

    pub(super) fn preview_col_idx(&self, result: &QueryResult) -> usize {
        self.row_identity
            .as_ref()
            .and_then(|ident| ident.columns.first())
            .and_then(|key| {
                result
                    .columns
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(key))
            })
            .unwrap_or(0)
    }

    /// 二进制显示值无法无损回写，必须只读。
    pub(in crate::views) fn cell_is_binary(&self, ri: usize, ci: usize) -> bool {
        matches!(
            self.cell_value(ri, ci),
            Some(ramag_domain::entities::Value::Bytes(_))
        )
    }
}
