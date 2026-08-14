//! 快捷键中心与最近项目通用弹窗。

mod bindings;
mod common;

use gpui::{
    App, AppContext as _, ClickEvent, Context, InteractiveElement as _, IntoElement, Keystroke,
    ParentElement, Render, Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, scroll::ScrollableElement as _, v_flex,
};

pub const SHORTCUT_OVERRIDES_PREF_KEY: &str = "shortcut_overrides_v1";

#[derive(Clone, Copy)]
pub struct ShortcutSpec {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub action: Option<&'static str>,
    pub context: Option<&'static str>,
    pub default_key: &'static str,
    pub macos: &'static str,
    pub windows: &'static str,
    pub linux: &'static str,
}

macro_rules! shortcut {
    ($id:literal, $group:literal, $label:literal, $action:literal, $context:expr, $key:literal, $mac:literal, $other:literal) => {
        ShortcutSpec {
            id: $id,
            group: $group,
            label: $label,
            action: Some($action),
            context: $context,
            default_key: $key,
            macos: $mac,
            windows: $other,
            linux: $other,
        }
    };
}

const SHORTCUTS: &[ShortcutSpec] = &[
    shortcut!(
        "open-recent",
        "全局",
        "打开连接选择器",
        "ramag::OpenRecentItems",
        None,
        "secondary-p",
        "⌘P",
        "Ctrl+P"
    ),
    ShortcutSpec {
        id: "wake-main-window",
        group: "全局",
        label: "全局唤醒 Ramag",
        action: None,
        context: None,
        default_key: "secondary-alt-shift-v",
        macos: "⌘⌥⇧V",
        windows: "Ctrl+Alt+Shift+V",
        linux: "暂不支持",
    },
    shortcut!(
        "close-tab",
        "全局",
        "关闭标签",
        "ramag::CloseTab",
        None,
        "secondary-w",
        "⌘W",
        "Ctrl+W"
    ),
    shortcut!(
        "quit",
        "全局",
        "退出应用",
        "ramag::Quit",
        None,
        "secondary-q",
        "⌘Q",
        "Ctrl+Q"
    ),
    shortcut!(
        "run-query",
        "数据库",
        "执行查询",
        "ramag_dbclient::RunQuery",
        None,
        "secondary-enter",
        "⌘Enter",
        "Ctrl+Enter"
    ),
    shortcut!(
        "run-statement",
        "数据库",
        "执行光标所在语句",
        "ramag_dbclient::RunStatementAtCursor",
        None,
        "secondary-shift-enter",
        "⌘⇧Enter",
        "Ctrl+Shift+Enter"
    ),
    shortcut!(
        "new-query",
        "数据库",
        "新建查询标签",
        "ramag_dbclient::NewQueryTab",
        None,
        "secondary-t",
        "⌘T",
        "Ctrl+T"
    ),
    shortcut!(
        "find-results",
        "数据库",
        "结果筛选",
        "ramag_dbclient::FindInResults",
        None,
        "secondary-f",
        "⌘F",
        "Ctrl+F"
    ),
    shortcut!(
        "format-sql",
        "数据库",
        "格式化 SQL",
        "ramag_dbclient::FormatSql",
        None,
        "secondary-shift-f",
        "⌘⇧F",
        "Ctrl+Shift+F"
    ),
    shortcut!(
        "toggle-editor",
        "数据库",
        "显示/隐藏编辑器",
        "ramag_dbclient::ToggleSqlEditor",
        None,
        "secondary-e",
        "⌘E",
        "Ctrl+E"
    ),
    shortcut!(
        "toggle-redis-console",
        "数据库",
        "显示/隐藏 Redis 控制台",
        "ramag_redis::ToggleRedisConsole",
        Some("RedisSession"),
        "secondary-e",
        "⌘E",
        "Ctrl+E"
    ),
    shortcut!(
        "vcs-commit",
        "Git",
        "提交",
        "ramag_vcs::CommitNow",
        Some("VcsView"),
        "secondary-enter",
        "⌘Enter",
        "Ctrl+Enter"
    ),
    shortcut!(
        "vcs-push",
        "Git",
        "Push",
        "ramag_vcs::PushNow",
        Some("VcsView"),
        "secondary-shift-k",
        "⌘⇧K",
        "Ctrl+Shift+K"
    ),
    shortcut!(
        "vcs-pull",
        "Git",
        "Pull",
        "ramag_vcs::PullNow",
        Some("VcsView"),
        "secondary-t",
        "⌘T",
        "Ctrl+T"
    ),
    shortcut!(
        "ssh-new-terminal",
        "SSH",
        "新建终端",
        "ssh::NewSshTerminal",
        Some("SshWorkspace"),
        "secondary-t",
        "⌘T",
        "Ctrl+T"
    ),
    shortcut!(
        "ssh-close-terminal",
        "SSH",
        "关闭终端",
        "ssh::CloseSshTerminal",
        Some("SshWorkspace"),
        "secondary-w",
        "⌘W",
        "Ctrl+W"
    ),
];

const MODULE_GROUPS: &[&str] = &["数据库", "Git", "SSH", "对象存储", "剪贴板"];

pub use bindings::{apply_saved_shortcut_overrides, init_shortcut_overrides};
use bindings::{
    display_key, overrides, platform_defaults, platform_name, reset_override, reset_overrides,
    serialize_keystroke, set_override, valid_recorded_keystroke,
};
use common::{render_common_group, render_type_heading};

pub fn open_shortcuts(window: &mut Window, cx: &mut App) {
    let panel = cx.new(ShortcutPanel::new);
    window.open_dialog(cx, move |dialog, window, _| {
        let panel = panel.clone();
        let dialog_max_h = (window.viewport_size().height * 0.86)
            .max(px(420.0))
            .min(px(820.0));
        dialog
            .title(crate::closable_dialog_title(
                "ramag-shortcuts-close",
                "快捷键",
                |_, _| {},
            ))
            .close_button(false)
            .w(px(820.0))
            .max_h(dialog_max_h)
            .margin_top(px(42.0))
            .content(move |content, _, _| content.child(panel.clone()))
    });
}

struct ShortcutPanel {
    recording: Option<&'static str>,
    error: Option<String>,
    interceptor: Option<Subscription>,
}

impl ShortcutPanel {
    fn new(_: &mut Context<Self>) -> Self {
        Self {
            recording: None,
            error: None,
            interceptor: None,
        }
    }

    fn start_recording(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.recording = Some(id);
        self.error = None;
        let listener = cx.listener(|this, event: &gpui::KeystrokeEvent, _, cx| {
            this.capture(event.keystroke.clone(), cx);
        });
        self.interceptor = Some(cx.intercept_keystrokes(listener));
        cx.notify();
    }

    fn capture(&mut self, keystroke: Keystroke, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if keystroke.key.eq_ignore_ascii_case("escape") {
            self.finish_recording(cx);
            return;
        }
        let Some(id) = self.recording else {
            return;
        };
        if !valid_recorded_keystroke(&keystroke) {
            self.error = Some("请使用带修饰键的组合，或 Enter、Tab、Delete、方向键、F1–F12".into());
            cx.notify();
            return;
        }
        let raw = serialize_keystroke(&keystroke);
        match set_override(id, &raw, cx) {
            Ok(()) => self.finish_recording(cx),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn finish_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.interceptor.take();
        cx.notify();
    }

    fn reset_one(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if let Err(error) = reset_override(id, cx) {
            self.error = Some(error);
        } else {
            self.error = None;
        }
        cx.notify();
    }

    fn reset_all(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = reset_overrides(cx) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.error = None;
        cx.notify();
    }

    fn render_group(
        &self,
        group: &'static str,
        show_title: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let overrides = overrides(cx);
        let specs: Vec<_> = SHORTCUTS
            .iter()
            .filter(|spec| spec.group == group)
            .collect();
        let mut rows = v_flex()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.0))
            .overflow_hidden();
        for (index, spec) in specs.iter().enumerate() {
            let custom = overrides.get(spec.id);
            let key = custom.map(String::as_str).unwrap_or(spec.default_key);
            let recording = self.recording == Some(spec.id);
            let id = spec.id;
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
                                    .child(spec.label),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(platform_defaults(spec)),
                            ),
                    )
                    .child(if spec.action.is_some() {
                        h_flex()
                            .gap(px(5.0))
                            .child(
                                crate::clickable_button(format!("shortcut-record-{}", spec.id))
                                    .outline()
                                    .small()
                                    .min_w(px(150.0))
                                    .label(if recording {
                                        "请按下组合键…".into()
                                    } else {
                                        display_key(key)
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.start_recording(id, cx);
                                    })),
                            )
                            .when(custom.is_some(), |actions| {
                                actions.child(
                                    crate::clickable_button(format!("shortcut-reset-{}", spec.id))
                                        .ghost()
                                        .small()
                                        .icon(IconName::Undo2)
                                        .tooltip("恢复默认")
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, _, cx| {
                                                this.reset_one(id, cx);
                                            },
                                        )),
                                )
                            })
                            .into_any_element()
                    } else {
                        h_flex()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("系统级"),
                            )
                            .child(shortcut_pill(current_platform_default(spec), theme))
                            .into_any_element()
                    }),
            );
        }
        v_flex()
            .w_full()
            .gap(px(8.0))
            .when(show_title, |section| {
                section.child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(group),
                )
            })
            .child(rows)
    }
}

impl Render for ShortcutPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let list_height = (window.viewport_size().height * 0.62)
            .max(px(280.0))
            .min(px(640.0));
        let has_overrides = !overrides(cx).is_empty();
        let global_group = v_flex()
            .w_full()
            .gap(px(10.0))
            .child(render_type_heading("全局", cx.theme()))
            .child(self.render_group("全局", false, cx));
        let mut module_groups = v_flex()
            .w_full()
            .gap(px(20.0))
            .child(render_type_heading("模块", cx.theme()));
        for group in MODULE_GROUPS
            .iter()
            .filter(|group| SHORTCUTS.iter().any(|spec| spec.group == **group))
        {
            module_groups = module_groups.child(self.render_group(group, true, cx));
        }
        let groups = v_flex()
            .w_full()
            .gap(px(28.0))
            .child(global_group)
            .child(module_groups)
            .child(render_common_group(cx.theme()));
        v_flex()
            .w_full()
            .gap(px(14.0))
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .gap(px(16.0))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("快捷键中心"),
                            )
                            .child(div().text_sm().text_color(muted).child(format!(
                                "当前系统：{} · 键盘快捷键可点击修改，常用操作为固定交互",
                                platform_name()
                            ))),
                    )
                    .child(
                        crate::clickable_button("shortcut-reset-all")
                            .ghost()
                            .small()
                            .label("全部恢复默认")
                            .disabled(!has_overrides)
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.reset_all(cx)),
                            ),
                    ),
            )
            .when_some(self.error.clone(), |body, error| {
                body.child(
                    div()
                        .w_full()
                        .px(px(10.0))
                        .py(px(7.0))
                        .rounded(px(6.0))
                        .bg(danger.opacity(0.08))
                        .text_xs()
                        .text_color(danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .id("shortcut-center-scroll")
                    .w_full()
                    .h(list_height)
                    .overflow_y_scrollbar()
                    .pr(px(5.0))
                    .child(groups),
            )
    }
}

fn current_platform_default(spec: &ShortcutSpec) -> &'static str {
    if cfg!(target_os = "macos") {
        spec.macos
    } else if cfg!(target_os = "windows") {
        spec.windows
    } else {
        spec.linux
    }
}

fn shortcut_pill(value: &str, theme: &gpui_component::Theme) -> impl IntoElement {
    div()
        .min_w(px(150.0))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .text_center()
        .child(value.to_string())
}

pub fn shortcut_icon() -> Icon {
    Icon::default().path("icons/keyboard.svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_letters_are_rejected_but_navigation_keys_are_allowed() {
        assert!(matches!(
            Keystroke::parse("a"),
            Ok(keystroke) if !valid_recorded_keystroke(&keystroke)
        ));
        assert!(matches!(
            Keystroke::parse("enter"),
            Ok(keystroke) if valid_recorded_keystroke(&keystroke)
        ));
        assert!(matches!(
            Keystroke::parse("ctrl-a"),
            Ok(keystroke) if valid_recorded_keystroke(&keystroke)
        ));
    }

    #[test]
    fn every_editable_shortcut_has_a_unique_id_and_valid_default() {
        let mut ids = std::collections::HashSet::new();
        for spec in SHORTCUTS {
            assert!(ids.insert(spec.id));
            assert!(Keystroke::parse(spec.default_key).is_ok());
        }
    }

    #[test]
    fn default_description_only_mentions_current_platform_key() {
        let description = platform_defaults(&SHORTCUTS[0]);
        assert!(description.starts_with("默认："));
        assert!(!description.contains("macOS "));
        assert!(!description.contains("Windows "));
        assert!(!description.contains("Linux "));
    }

    #[test]
    fn global_wake_shortcut_is_visible_and_system_managed() {
        let shortcut = SHORTCUTS
            .iter()
            .find(|shortcut| shortcut.id == "wake-main-window");
        assert!(shortcut.is_some_and(|shortcut| {
            shortcut.group == "全局"
                && shortcut.action.is_none()
                && shortcut.macos == "⌘⌥⇧V"
                && shortcut.windows == "Ctrl+Alt+Shift+V"
        }));
    }

    #[test]
    fn every_keyboard_shortcut_belongs_to_global_or_module_type() {
        assert!(SHORTCUTS.iter().all(|shortcut| {
            shortcut.group == "全局" || MODULE_GROUPS.contains(&shortcut.group)
        }));
    }
}
