pub fn primary_shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

pub fn primary_shift_shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘⇧{key}")
    } else {
        format!("Ctrl+Shift+{key}")
    }
}

pub fn primary_alt_shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘⌥{key}")
    } else {
        format!("Ctrl+Alt+{key}")
    }
}

pub fn clipboard_hotkey(alternate: bool) -> String {
    if alternate {
        primary_alt_shortcut("V")
    } else {
        primary_shift_shortcut("V")
    }
}

pub fn file_manager_reveal_label() -> &'static str {
    "显示文件"
}

pub fn auto_paste_description() -> &'static str {
    if cfg!(target_os = "macos") {
        "选中后粘贴（需辅助功能权限）"
    } else {
        "选中后粘贴；管理员应用可能只能复制"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_labels_match_current_platform() {
        if cfg!(target_os = "macos") {
            assert_eq!(primary_shortcut("Enter"), "⌘Enter");
            assert_eq!(primary_shift_shortcut("F"), "⌘⇧F");
            assert_eq!(clipboard_hotkey(false), "⌘⇧V");
            assert_eq!(clipboard_hotkey(true), "⌘⌥V");
        } else {
            assert_eq!(primary_shortcut("Enter"), "Ctrl+Enter");
            assert_eq!(primary_shift_shortcut("F"), "Ctrl+Shift+F");
            assert_eq!(clipboard_hotkey(false), "Ctrl+Shift+V");
            assert_eq!(clipboard_hotkey(true), "Ctrl+Alt+V");
        }
    }
}
