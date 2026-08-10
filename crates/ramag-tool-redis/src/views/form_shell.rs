use std::collections::HashSet;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use gpui_component::{Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex};

#[derive(Debug, Clone)]
pub enum SubmitState {
    Idle,
    Submitting,
    Failed(String),
}

impl SubmitState {
    pub fn error(&self) -> Option<String> {
        match self {
            SubmitState::Failed(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn is_submitting(&self) -> bool {
        matches!(self, SubmitState::Submitting)
    }
}

/// 保留首次出现顺序地去重；HashSet 仅借用原字符串，避免批量输入再复制一份内容。
pub fn deduplicate_preserving_order(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::with_capacity(values.len());
    let keep: Vec<bool> = values
        .iter()
        .map(|value| seen.insert(value.as_str()))
        .collect();
    drop(seen);

    values
        .into_iter()
        .zip(keep)
        .filter_map(|(value, keep)| keep.then_some(value))
        .collect()
}

/// 提交中自动给主操作添加“中…”后缀。
pub fn form_footer<V: 'static>(
    id_prefix: &str,
    save_label: &str,
    state: &SubmitState,
    on_cancel: impl Fn(&mut V, &ClickEvent, &mut Window, &mut Context<V>) + 'static,
    on_save: impl Fn(&mut V, &ClickEvent, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let save_text = if state.is_submitting() {
        format!("{save_label}中…")
    } else {
        save_label.to_string()
    };
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(gpui::red())
                .child(state.error().unwrap_or_default()),
        )
        .child(
            h_flex()
                .gap(px(8.0))
                .flex_none()
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!("{id_prefix}-cancel")))
                        .ghost()
                        .small()
                        .label("取消")
                        .disabled(state.is_submitting())
                        .on_click(cx.listener(on_cancel)),
                )
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!("{id_prefix}-save")))
                        .primary()
                        .small()
                        .label(save_text)
                        .disabled(state.is_submitting())
                        .on_click(cx.listener(on_save)),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::deduplicate_preserving_order;

    #[test]
    fn deduplication_keeps_first_value_and_order() {
        let values = vec!["a".into(), "b".into(), "a".into(), "c".into()];

        assert_eq!(deduplicate_preserving_order(values), vec!["a", "b", "c"]);
    }
}
