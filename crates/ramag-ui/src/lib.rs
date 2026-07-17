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
pub mod preferences;
pub mod prompt_dialog;
pub mod resizable_persist;
pub mod settings_view;
pub mod shell;
pub mod theme;

pub use actions::{
    CloseTab, CycleSection, CycleSectionReverse, SelectTool1, SelectTool2, SelectTool3,
    ShowOnboarding,
};
pub use assets::RamagAssets;
pub use confirm_dialog::open_confirm;
pub use prompt_dialog::{
    open_bounded_prompt, open_optional_bounded_prompt, open_optional_prompt, open_prompt,
};

pub use activity_bar::{ActivityBar, NavEvent, NavTarget};
pub use editor_workspace::{
    EditorDraftPref, EditorWorkspacePref, MAX_EDITOR_DRAFT_BYTES, MAX_EDITOR_TABS,
    MAX_EDITOR_WORKSPACE_PREF_BYTES, MAX_EDITOR_WORKSPACE_TEXT_BYTES, can_open_editor_tab,
};
pub use home_view::{HomeEvent, HomeView};
pub use mutation_gate::{AsyncMutationGate, MutationToken};
pub use resizable_persist::persist_resizable_sizes;
pub use settings_view::{SettingsEvent, SettingsView};
pub use shell::{Shell, WindowBoundsPref};
pub use theme::{
    Mode, StorageGlobal, apply_theme, current_mode, init_theme, on_system_appearance_changed,
};

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
    use super::byte_prefix;

    #[test]
    fn byte_prefix_preserves_utf8_boundaries() {
        assert_eq!(byte_prefix("你好世界", 7), "你好");
        assert_eq!(byte_prefix("abc", 99), "abc");
        assert_eq!(byte_prefix("abc", 0), "");
    }
}
