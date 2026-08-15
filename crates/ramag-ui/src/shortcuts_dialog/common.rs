//! 常用鼠标操作说明。

use gpui::{IntoElement, ParentElement as _, Styled as _, div, prelude::FluentBuilder as _, px};
use gpui_component::{h_flex, v_flex};

#[derive(Clone, Copy)]
struct CommonInteraction {
    label: &'static str,
    description: &'static str,
    macos_gesture: &'static str,
    other_gesture: &'static str,
}

const COMMON_INTERACTIONS: &[CommonInteraction] = &[
    CommonInteraction {
        label: "执行操作",
        description: "打开、编辑或应用当前内容",
        macos_gesture: "双击",
        other_gesture: "双击",
    },
    CommonInteraction {
        label: "复制完整值",
        description: "复制未截断的单元格或文件路径",
        macos_gesture: "⌘ + 双击",
        other_gesture: "Ctrl + 双击",
    },
    CommonInteraction {
        label: "更多操作",
        description: "打开操作菜单",
        macos_gesture: "右键",
        other_gesture: "右键",
    },
];

pub(super) fn render_common_group(theme: &gpui_component::Theme) -> impl IntoElement {
    let mut rows = v_flex()
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded(px(8.0))
        .overflow_hidden();
    for (index, interaction) in COMMON_INTERACTIONS.iter().enumerate() {
        rows = rows.child(
            h_flex()
                .w_full()
                .min_h(px(58.0))
                .items_center()
                .gap(px(14.0))
                .px(px(14.0))
                .py(px(8.0))
                .when(index > 0, |row| row.border_t_1().border_color(theme.border))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(3.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(interaction.label),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(interaction.description),
                        ),
                )
                .child(super::shortcut_pill(current_gesture(interaction), theme)),
        );
    }
    v_flex()
        .w_full()
        .gap(px(10.0))
        .child(render_type_heading("常用", theme))
        .child(rows)
}

pub(super) fn render_type_heading(
    title: &'static str,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(6.0))
        .border_l_1()
        .border_color(theme.accent)
        .bg(theme.accent.opacity(0.1))
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.accent)
                .child(title),
        )
}

fn current_gesture(interaction: &CommonInteraction) -> &'static str {
    if cfg!(target_os = "macos") {
        interaction.macos_gesture
    } else {
        interaction.other_gesture
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_interactions_cover_double_click_copy_and_context_menu() {
        assert_eq!(COMMON_INTERACTIONS.len(), 3);
        assert_eq!(COMMON_INTERACTIONS[0].macos_gesture, "双击");
        assert_eq!(COMMON_INTERACTIONS[1].macos_gesture, "⌘ + 双击");
        assert_eq!(COMMON_INTERACTIONS[1].other_gesture, "Ctrl + 双击");
        assert_eq!(COMMON_INTERACTIONS[2].other_gesture, "右键");
    }
}
