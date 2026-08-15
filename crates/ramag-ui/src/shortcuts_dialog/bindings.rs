//! 快捷键录制、冲突校验与动态绑定。

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{App, KeyBinding, KeyBindingContextPredicate, Keystroke, SharedString, Unbind};

use super::{SHORTCUT_OVERRIDES_PREF_KEY, SHORTCUTS, ShortcutSpec};

#[derive(Clone, Default)]
struct ShortcutOverrides(HashMap<String, String>);
impl gpui::Global for ShortcutOverrides {}

pub(super) fn overrides(cx: &App) -> HashMap<String, String> {
    cx.try_global::<ShortcutOverrides>()
        .map(|global| global.0.clone())
        .unwrap_or_default()
}

pub fn init_shortcut_overrides(value: Option<&str>, cx: &mut App) {
    let parsed = value
        .filter(|value| value.len() <= 64 * 1024)
        .and_then(|value| serde_json::from_str::<HashMap<String, String>>(value).ok())
        .unwrap_or_default();
    let had_removed_shortcuts = parsed.keys().any(|id| find_spec(id).is_none());
    let values: HashMap<_, _> = parsed
        .into_iter()
        .filter(|(id, key)| find_spec(id).is_some() && Keystroke::parse(key).is_ok())
        .collect();
    cx.set_global(ShortcutOverrides(values.clone()));
    if had_removed_shortcuts {
        match serde_json::to_string(&values) {
            Ok(json) => {
                crate::preferences::persist_preference_latest(SHORTCUT_OVERRIDES_PREF_KEY, json, cx)
            }
            Err(error) => tracing::warn!(
                operation = "shortcut_removed_overrides_cleanup",
                error = %error,
                "serialize cleaned shortcut overrides failed"
            ),
        }
    }
}

pub fn apply_saved_shortcut_overrides(cx: &mut App) {
    for (id, key) in overrides(cx) {
        if let Some(spec) = find_spec(&id)
            && key != spec.default_key
            && let Err(error) = append_rebinding(spec, spec.default_key, &key, cx)
        {
            tracing::warn!(
                operation = "shortcut_override_apply",
                shortcut_id = id,
                error,
                "apply shortcut override failed"
            );
        }
    }
}

pub(super) fn set_override(id: &str, key: &str, cx: &mut App) -> Result<(), String> {
    let spec = find_spec(id).ok_or_else(|| "未知快捷键".to_string())?;
    if spec.action.is_none() {
        return Err("系统级快捷键请在对应工具设置中修改".into());
    }
    validate_conflict(spec, key, cx)?;
    let mut values = overrides(cx);
    let previous = values
        .get(id)
        .map(String::as_str)
        .unwrap_or(spec.default_key)
        .to_string();
    append_rebinding(spec, &previous, key, cx)?;
    if key == spec.default_key {
        values.remove(id);
    } else {
        values.insert(id.to_string(), key.to_string());
    }
    persist_overrides(values, cx)
}

pub(super) fn reset_override(id: &str, cx: &mut App) -> Result<(), String> {
    let spec = find_spec(id).ok_or_else(|| "未知快捷键".to_string())?;
    let mut values = overrides(cx);
    let Some(previous) = values.remove(id) else {
        return Ok(());
    };
    append_rebinding(spec, &previous, spec.default_key, cx)?;
    persist_overrides(values, cx)
}

pub(super) fn reset_overrides(cx: &mut App) -> Result<(), String> {
    let ids: Vec<_> = overrides(cx).into_keys().collect();
    for id in ids {
        reset_override(&id, cx)?;
    }
    Ok(())
}

fn persist_overrides(values: HashMap<String, String>, cx: &mut App) -> Result<(), String> {
    let json =
        serde_json::to_string(&values).map_err(|error| format!("保存快捷键失败：{error}"))?;
    cx.set_global(ShortcutOverrides(values));
    crate::preferences::persist_preference_latest(SHORTCUT_OVERRIDES_PREF_KEY, json, cx);
    Ok(())
}

fn append_rebinding(
    spec: &ShortcutSpec,
    previous: &str,
    next: &str,
    cx: &mut App,
) -> Result<(), String> {
    let action_name = spec.action.ok_or_else(|| "该快捷键不可编辑".to_string())?;
    let predicate = spec
        .context
        .map(KeyBindingContextPredicate::parse)
        .transpose()
        .map_err(|error| format!("快捷键上下文无效：{error}"))?
        .map(Rc::new);
    let unbind = KeyBinding::load(
        previous,
        Box::new(Unbind(SharedString::from(action_name))),
        predicate.clone(),
        false,
        None,
        cx.keyboard_mapper().as_ref(),
    )
    .map_err(|error| format!("旧快捷键无效：{error}"))?;
    let action = cx
        .build_action(action_name, None)
        .map_err(|error| format!("无法加载快捷键操作：{error}"))?;
    let binding = KeyBinding::load(
        next,
        action,
        predicate,
        false,
        None,
        cx.keyboard_mapper().as_ref(),
    )
    .map_err(|error| format!("快捷键无效：{error}"))?;
    cx.bind_keys([unbind, binding]);
    Ok(())
}

fn validate_conflict(spec: &ShortcutSpec, key: &str, cx: &App) -> Result<(), String> {
    Keystroke::parse(key).map_err(|error| format!("快捷键无效：{error}"))?;
    let values = overrides(cx);
    if let Some(conflict) = SHORTCUTS.iter().find(|other| {
        other.id != spec.id
            && other.action.is_some()
            && other.context == spec.context
            && values
                .get(other.id)
                .map(String::as_str)
                .unwrap_or(other.default_key)
                == key
    }) {
        return Err(format!("与“{}”冲突，请换一个组合键", conflict.label));
    }
    Ok(())
}

pub(super) fn valid_recorded_keystroke(keystroke: &Keystroke) -> bool {
    if keystroke.modifiers.modified() {
        return !keystroke.key.is_empty();
    }
    let key = keystroke.key.to_lowercase();
    matches!(
        key.as_str(),
        "enter" | "tab" | "delete" | "backspace" | "up" | "down" | "left" | "right"
    ) || key
        .strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=12).contains(&number))
}

pub(super) fn serialize_keystroke(keystroke: &Keystroke) -> String {
    let mut parts = Vec::new();
    if keystroke.modifiers.control {
        parts.push("ctrl".to_string());
    }
    if keystroke.modifiers.alt {
        parts.push("alt".to_string());
    }
    if keystroke.modifiers.shift {
        parts.push("shift".to_string());
    }
    if keystroke.modifiers.platform {
        parts.push(
            if cfg!(target_os = "macos") {
                "cmd"
            } else {
                "super"
            }
            .to_string(),
        );
    }
    if keystroke.modifiers.function {
        parts.push("fn".to_string());
    }
    parts.push(keystroke.key.to_lowercase());
    parts.join("-")
}

pub(super) fn display_key(raw: &str) -> String {
    Keystroke::parse(raw)
        .map(|keystroke| keystroke.to_string())
        .unwrap_or_else(|_| raw.to_string())
}

pub(super) fn platform_defaults(spec: &ShortcutSpec) -> String {
    let key = if cfg!(target_os = "macos") {
        spec.macos
    } else if cfg!(target_os = "windows") {
        spec.windows
    } else {
        spec.linux
    };
    format!("默认：{key}")
}

pub(super) fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    }
}

fn find_spec(id: &str) -> Option<&'static ShortcutSpec> {
    SHORTCUTS.iter().find(|spec| spec.id == id)
}
