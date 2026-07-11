//! 用户可见的平台术语与快捷键文案。

/// 主修饰键快捷键：macOS 使用 Command，Windows 使用 Ctrl。
pub fn primary_shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

/// 主修饰键 + Shift 快捷键。
pub fn primary_shift_shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘⇧{key}")
    } else {
        format!("Ctrl+Shift+{key}")
    }
}

pub fn clipboard_hotkey() -> String {
    primary_shift_shortcut("V")
}

pub fn file_manager_reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "在 Finder 显示"
    } else {
        "在文件资源管理器中显示"
    }
}

pub fn auto_paste_description() -> &'static str {
    if cfg!(target_os = "macos") {
        "抽屉选中后自动粘贴到当前应用（需辅助功能权限）"
    } else {
        "抽屉选中后自动切回原窗口并粘贴（管理员权限应用可能仅复制）"
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
        } else {
            assert_eq!(primary_shortcut("Enter"), "Ctrl+Enter");
            assert_eq!(primary_shift_shortcut("F"), "Ctrl+Shift+F");
        }
    }
}
