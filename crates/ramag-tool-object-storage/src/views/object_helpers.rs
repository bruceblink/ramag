use ramag_domain::entities::is_opendal_safe_prefix;

pub(super) fn format_object_preview(key: &str, content: &str) -> String {
    if key
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("json"))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
        && let Ok(formatted) = serde_json::to_string_pretty(&value)
    {
        return formatted;
    }
    content.to_string()
}

pub(super) fn object_preview_language(key: &str, content: &str) -> &'static str {
    let extension = key
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("rs") => "rust",
        Some("go") => "go",
        Some("py") => "python",
        Some("json") => "json",
        Some("jsonl" | "log") if looks_like_json_lines(content) => "json",
        Some("js" | "jsx") => "javascript",
        Some("ts") => "typescript",
        Some("tsx") => "tsx",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("sql") => "sql",
        Some("md") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp") => "cpp",
        Some("java") => "java",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        _ => "text",
    }
}

fn looks_like_json_lines(content: &str) -> bool {
    let mut lines = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(8);
    let Some(first) = lines.next() else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(first).is_ok()
        && lines.all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
}

pub(super) fn normalize_object_path(path: &str) -> Result<String, String> {
    if !path.starts_with('/') {
        return Err("对象路径必须以 / 开头".into());
    }
    let relative = path.strip_prefix('/').unwrap_or_default();
    if relative.is_empty() {
        return Ok(String::new());
    }
    let prefix = if relative.ends_with('/') {
        relative.to_string()
    } else {
        format!("{relative}/")
    };
    if !is_opendal_safe_prefix(&prefix) {
        return Err("对象路径包含不安全或无法识别的路径段".into());
    }
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::{format_object_preview, normalize_object_path, object_preview_language};

    #[test]
    fn direct_object_path_is_absolute_safe_and_normalized() {
        assert_eq!(normalize_object_path("/").as_deref(), Ok(""));
        assert_eq!(
            normalize_object_path("/gewu/structure").as_deref(),
            Ok("gewu/structure/")
        );
        assert!(normalize_object_path("gewu/structure").is_err());
        assert!(normalize_object_path("/gewu/../secret").is_err());
    }

    #[test]
    fn json_preview_is_formatted_and_uses_json_language() {
        let content = format_object_preview("config.json", "{\"enabled\":true}");
        assert!(content.contains('\n'));
        assert_eq!(object_preview_language("config.json", &content), "json");
    }
}
