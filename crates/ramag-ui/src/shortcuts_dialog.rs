//! 快捷键中心与最近项目通用弹窗。

mod bindings;
mod catalog;
mod common;

use gpui::{
    App, AppContext as _, ClickEvent, Context, InteractiveElement as _, IntoElement, Keystroke,
    ParentElement, Render, StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, v_flex,
};

pub const SHORTCUT_OVERRIDES_PREF_KEY: &str = "shortcut_overrides_v1";

pub use bindings::{apply_saved_shortcut_overrides, init_shortcut_overrides};
use bindings::{
    display_key, overrides, platform_defaults, platform_name, reset_override, reset_overrides,
    serialize_keystroke, set_override, valid_recorded_keystroke,
};
pub use catalog::ShortcutSpec;
use catalog::{MODULE_GROUPS, SHORTCUTS};
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
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("当前系统：{} · 点击快捷键修改", platform_name())),
                    )
                    .child(
                        crate::clickable_button("shortcut-reset-all")
                            .ghost()
                            .small()
                            .label("全部重置")
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
                    .overflow_y_scroll()
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
