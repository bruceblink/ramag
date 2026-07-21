//! 共享 UI：Shell（左 ActivityBar + 右 Tool 视图）+ 主题 + 通用组件

pub mod actions;
pub mod activity_bar;
pub mod assets;
pub mod confirm_dialog;
pub mod editor_workspace;
pub mod home_view;
pub mod icons;
pub mod mutation_gate;
pub mod platform;
pub mod pointer_menu;
pub mod preferences;
pub mod prompt_dialog;
pub mod resizable_persist;
pub mod settings_view;
pub mod shell;
pub mod theme;
pub mod transfer_ui;

pub use actions::{
    CloseTab, CycleSection, CycleSectionReverse, SelectTool1, SelectTool2, SelectTool3,
};
pub use assets::RamagAssets;
pub use confirm_dialog::open_confirm;
pub use prompt_dialog::{
    open_bounded_masked_prompt, open_bounded_prompt, open_masked_prompt,
    open_optional_bounded_prompt, open_optional_prompt, open_prompt, open_reveal_masked_prompt,
};

pub use activity_bar::{ActivityBar, NavEvent, NavTarget};
pub use editor_workspace::{
    EditorDraftPref, EditorWorkspacePref, MAX_EDITOR_DRAFT_BYTES, MAX_EDITOR_TABS,
    MAX_EDITOR_WORKSPACE_PREF_BYTES, MAX_EDITOR_WORKSPACE_TEXT_BYTES, can_open_editor_tab,
};
pub use home_view::{HomeEvent, HomeView};
pub use mutation_gate::{AsyncMutationGate, MutationToken};
pub use pointer_menu::PointerDropdownMenu;
pub use resizable_persist::persist_resizable_sizes;
pub use settings_view::SettingsView;
pub use shell::{Shell, WindowBoundsPref};
pub use theme::{Mode, StorageGlobal, apply_theme, current_mode, init_theme};
pub use transfer_ui::{
    TransferState, open_import_options_dialog, progress_sink, spawn_transfer_ticker,
    transfer_notification, transfer_progress_row,
};

/// 创建带手型光标的按钮，统一可点击控件的悬浮反馈。
pub fn clickable_button(id: impl Into<gpui::ElementId>) -> gpui_component::button::Button {
    use gpui::Styled as _;

    gpui_component::button::Button::new(id).cursor_pointer()
}

/// 创建带手型光标的复选框。
pub fn clickable_checkbox(id: impl Into<gpui::ElementId>) -> gpui_component::checkbox::Checkbox {
    use gpui::Styled as _;

    gpui_component::checkbox::Checkbox::new(id).cursor_pointer()
}

/// 创建带手型光标的开关。
pub fn clickable_switch(id: impl Into<gpui::ElementId>) -> gpui_component::switch::Switch {
    use gpui::Styled as _;

    gpui_component::switch::Switch::new(id).cursor_pointer()
}

/// 创建带手型清除按钮的单行输入框。
pub fn cleanable_input(
    state: &gpui::Entity<gpui_component::input::InputState>,
    clear_id: impl Into<gpui::ElementId>,
    disabled: bool,
    cx: &gpui::App,
) -> gpui_component::input::Input {
    use gpui::Styled as _;
    use gpui_component::{
        ActiveTheme as _, Icon, IconName, Sizable as _, button::ButtonVariants as _, input::Input,
    };

    let input = Input::new(state).disabled(disabled);
    if disabled || state.read(cx).value().is_empty() {
        return input;
    }

    let state = state.clone();
    input.suffix(
        clickable_button(clear_id)
            .icon(Icon::new(IconName::CircleX))
            .ghost()
            .xsmall()
            .tab_stop(false)
            .text_color(cx.theme().muted_foreground)
            .on_click(move |_, window, cx| {
                state.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                    state.focus(window, cx);
                });
            }),
    )
}

/// 创建带手型关闭按钮的对话框标题；调用方仍可用 `on_close` 处理 Esc 等关闭路径。
pub fn closable_dialog_title(
    id: impl Into<gpui::ElementId>,
    title: impl gpui::IntoElement,
    on_close: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> impl gpui::IntoElement {
    use gpui::{ParentElement as _, Styled as _};
    use gpui_component::{
        IconName, Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex,
    };

    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .child(title)
        .child(
            clickable_button(id)
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip("关闭")
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    on_close(window, cx);
                }),
        )
}

/// 创建带手型光标的菜单项。
pub fn menu_item(label: impl Into<gpui::SharedString>) -> gpui_component::menu::PopupMenuItem {
    menu_item_with_disabled(label, false)
}

/// 创建可禁用菜单项；禁用时保持箭头。
pub fn menu_item_with_disabled(
    label: impl Into<gpui::SharedString>,
    disabled: bool,
) -> gpui_component::menu::PopupMenuItem {
    use gpui::{ParentElement as _, Styled as _, div, prelude::FluentBuilder as _};

    let label = label.into();
    gpui_component::menu::PopupMenuItem::element(move |_, _| {
        div()
            .w_full()
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .when(disabled, |this| this.cursor_default())
                    .when(!disabled, |this| this.cursor_pointer()),
            )
            .child(label.clone())
    })
    .disabled(disabled)
}

/// 即时搜索 / 过滤会在每次输入后重算，限制异常粘贴造成的重复分配与全表扫描成本。
pub const MAX_SEARCH_INPUT_BYTES: usize = 4 * 1024;

pub fn bounded_search_input(
    window: &mut gpui::Window,
    cx: &mut gpui::Context<gpui_component::input::InputState>,
) -> gpui_component::input::InputState {
    gpui_component::input::InputState::new(window, cx)
        .validate(|value, _| value.len() <= MAX_SEARCH_INPUT_BYTES)
}

/// GPUI Component 的 `validate` 目前仅检查单行输入；多行 / 代码编辑器需在变更后立即收口。
/// 超限时保留 UTF-8 安全前缀，并调用 `on_exceeded` 让业务层展示具体提示。
pub fn enforce_multiline_input_byte_limit<T: 'static>(
    input: &gpui::Entity<gpui_component::input::InputState>,
    max_bytes: usize,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<T>,
    on_exceeded: impl Fn(&mut T, &mut gpui::Window, &mut gpui::Context<T>) + 'static,
) -> gpui::Subscription {
    let input_for_event = input.clone();
    cx.subscribe_in(
        input,
        window,
        move |this, _, event: &gpui_component::input::InputEvent, window, cx| {
            if !matches!(event, gpui_component::input::InputEvent::Change) {
                return;
            }
            if !clamp_multiline_input_value(&input_for_event, max_bytes, window, cx) {
                return;
            }
            on_exceeded(this, window, cx);
        },
    )
}

/// 将多行输入立即截到 UTF-8 安全字节边界；发生截断时返回 `true`。
pub fn clamp_multiline_input_value(
    input: &gpui::Entity<gpui_component::input::InputState>,
    max_bytes: usize,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> bool {
    let value = input.read(cx).value();
    if value.len() <= max_bytes {
        return false;
    }
    let bounded = byte_prefix(&value, max_bytes).to_string();
    input.update(cx, |state, cx| {
        state.set_value(bounded, window, cx);
    });
    true
}

fn byte_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod input_limit_tests {
    use super::{
        byte_prefix, clickable_button, clickable_checkbox, clickable_switch,
        menu_item_with_disabled,
    };
    use gpui::{CursorStyle, Styled as _};
    use gpui_component::menu::PopupMenuItem;

    #[test]
    fn byte_prefix_preserves_utf8_boundaries() {
        assert_eq!(byte_prefix("你好世界", 7), "你好");
        assert_eq!(byte_prefix("abc", 99), "abc");
        assert_eq!(byte_prefix("abc", 0), "");
    }

    #[test]
    fn clickable_components_use_pointing_hand_cursor() {
        let mut button = clickable_button("cursor-test-button");
        let mut checkbox = clickable_checkbox("cursor-test-checkbox");
        let mut switch = clickable_switch("cursor-test-switch");

        assert_eq!(button.style().mouse_cursor, Some(CursorStyle::PointingHand));
        assert_eq!(
            checkbox.style().mouse_cursor,
            Some(CursorStyle::PointingHand)
        );
        assert_eq!(switch.style().mouse_cursor, Some(CursorStyle::PointingHand));
    }

    #[test]
    fn menu_item_preserves_disabled_state() {
        let enabled = menu_item_with_disabled("enabled", false);
        let disabled = menu_item_with_disabled("disabled", true);

        assert!(matches!(
            enabled,
            PopupMenuItem::ElementItem {
                disabled: false,
                ..
            }
        ));
        assert!(matches!(
            disabled,
            PopupMenuItem::ElementItem { disabled: true, .. }
        ));
    }
}
