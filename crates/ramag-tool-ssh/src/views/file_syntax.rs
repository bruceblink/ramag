//! 远程文件路径与内容到内置 tree-sitter 语言的映射。

pub(super) fn language_for_remote_file(path: &str, text: &str) -> &'static str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name {
        "Makefile" | "makefile" | "GNUmakefile" => return "make",
        "CMakeLists.txt" => return "cmake",
        // gpui-component 暂无 Dockerfile 语法，Shell 高亮比纯文本更接近其结构。
        "Dockerfile" | "dockerfile" => return "bash",
        _ => {}
    }
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("rs") => "rust",
        Some("go") => "go",
        Some("py" | "pyi") => "python",
        Some("json" | "jsonc") => "json",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("sql") => "sql",
        Some("md" | "markdown") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("c" | "h") => "c",
        Some("cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx") => "cpp",
        Some("java") => "java",
        Some("kt" | "kts") => "kotlin",
        Some("swift") => "swift",
        Some("rb") => "ruby",
        Some("php") => "php",
        Some("lua") => "lua",
        Some("scala" | "sbt") => "scala",
        Some("ex" | "exs") => "elixir",
        Some("cs") => "csharp",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("svelte") => "svelte",
        Some("astro") => "astro",
        Some("graphql" | "gql") => "graphql",
        Some("proto") => "proto",
        Some("zig") => "zig",
        Some("mk") => "make",
        Some("cmake") => "cmake",
        Some("diff" | "patch") => "diff",
        Some("log") if looks_like_json_lines(text) => "json",
        _ => "text",
    }
}

fn looks_like_json_lines(text: &str) -> bool {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty()).take(8);
    let Some(first) = lines.next() else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(first).is_ok()
        && lines.all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_extension_selects_builtin_language() {
        assert_eq!(language_for_remote_file("/srv/main.rs", ""), "rust");
        assert_eq!(language_for_remote_file("/srv/init-db.sql", ""), "sql");
        assert_eq!(language_for_remote_file("/srv/config.yml", ""), "yaml");
        assert_eq!(language_for_remote_file("/srv/unknown.bin", ""), "text");
    }

    #[test]
    fn json_lines_log_uses_json_highlighting() {
        let json_lines = "{\"level\":\"info\",\"msg\":\"ready\"}\n{\"level\":\"error\"}";
        assert_eq!(
            language_for_remote_file("/var/log/app.log", json_lines),
            "json"
        );
        assert_eq!(
            language_for_remote_file("/var/log/app.log", "INFO ready"),
            "text"
        );
    }
}
