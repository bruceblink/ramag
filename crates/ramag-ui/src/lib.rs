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
pub use prompt_dialog::open_prompt;

pub use activity_bar::{ActivityBar, NavEvent, NavTarget};
pub use editor_workspace::{EditorDraftPref, EditorWorkspacePref};
pub use home_view::{HomeEvent, HomeView};
pub use mutation_gate::{AsyncMutationGate, MutationToken};
pub use resizable_persist::persist_resizable_sizes;
pub use settings_view::{SettingsEvent, SettingsView};
pub use shell::{Shell, WindowBoundsPref};
pub use theme::{
    Mode, StorageGlobal, apply_theme, current_mode, init_theme, on_system_appearance_changed,
};
