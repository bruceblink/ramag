use super::*;
use ramag_domain::entities::{DiffLine, FileChangeKind, Hunk};

#[test]
fn maps_known_extensions() {
    assert_eq!(lang_for_path("src/main.rs"), Some("rust"));
    assert_eq!(lang_for_path("a/b/util.go"), Some("go"));
    assert_eq!(lang_for_path("script.py"), Some("python"));
    assert_eq!(lang_for_path("data.json"), Some("json"));
    assert_eq!(lang_for_path("mod.mjs"), Some("javascript"));
    assert_eq!(lang_for_path("config.yml"), Some("yaml"));
    assert_eq!(lang_for_path("header.hpp"), Some("cpp"));
    assert_eq!(lang_for_path("Main.java"), Some("java"));
    assert_eq!(lang_for_path("App.kt"), Some("kotlin"));
    assert_eq!(lang_for_path("View.swift"), Some("swift"));
    assert_eq!(lang_for_path("index.html"), Some("html"));
    assert_eq!(lang_for_path("style.css"), Some("css"));
    assert_eq!(lang_for_path("schema.graphql"), Some("graphql"));
    assert_eq!(lang_for_path("api.proto"), Some("proto"));
    assert_eq!(lang_for_path("fix.patch"), Some("diff"));
}

/// tsx 用独立 grammar（TSX 的 JSX 语法 typescript grammar 解析不了）
#[test]
fn tsx_uses_dedicated_grammar() {
    assert_eq!(lang_for_path("app.tsx"), Some("tsx"));
    assert_eq!(lang_for_path("util.ts"), Some("typescript"));
}

#[test]
fn filename_without_extension_matches() {
    assert_eq!(lang_for_path("Makefile"), Some("make"));
    assert_eq!(lang_for_path("scripts/GNUmakefile"), Some("make"));
    assert_eq!(lang_for_path("CMakeLists.txt"), Some("cmake"));
}

#[test]
fn case_insensitive_extension() {
    assert_eq!(lang_for_path("README.MD"), Some("markdown"));
    assert_eq!(lang_for_path("Build.SQL"), Some("sql"));
}

#[test]
fn unknown_or_no_extension_is_none() {
    // 无扩展名 / 仅前缀点 / 不在表内 → 纯文本
    assert_eq!(lang_for_path("Cargo.lock"), None);
    assert_eq!(lang_for_path(".gitignore"), None);
    assert_eq!(lang_for_path("path/to/dir.with.dots/file"), None);
}

#[test]
fn display_line_expands_tabs_and_keeps_long_utf8_text() {
    let short = prepare_display_line("a\tb");
    assert_eq!(short.text, "a   b");
    assert_eq!(short.cols, 5);

    let source = "中".repeat(MAX_HIGHLIGHT_LINE_BYTES);
    let long = prepare_display_line(&source);
    assert_eq!(long.text, source);
    assert_eq!(long.highlight_len, None);
}

#[test]
fn syntax_document_keeps_lines_and_bounded_width() {
    let lines = ["fn main() {", "\tprintln!(\"ok\");", "}"];
    let document = SyntaxDocument::new(lines, Some("rust"));
    assert_eq!(document.lines.len(), 3);
    assert_eq!(display_cols(lines[1]), 19);

    let theme = HighlightTheme::default_dark();
    let key = highlight_theme_key(&theme);
    let line = document.line(1, &theme, key);
    assert_eq!(
        line.as_ref().map(|line| line.text.as_ref()),
        Some("    println!(\"ok\");")
    );
    assert!(
        line.as_ref()
            .is_some_and(|line| !line.highlights.is_empty())
    );
}

#[test]
fn highlight_line_cache_is_bounded() {
    let mut cache = LineStyleCache {
        theme_key: Some(1),
        styles: HashMap::new(),
        order: VecDeque::new(),
    };
    for index in 0..MAX_CACHED_HIGHLIGHT_LINES + 5 {
        cache.insert(index, Vec::new());
    }

    assert_eq!(cache.styles.len(), MAX_CACHED_HIGHLIGHT_LINES);
    assert!(!cache.styles.contains_key(&0));
    assert!(cache.styles.contains_key(&(MAX_CACHED_HIGHLIGHT_LINES + 4)));
}

#[test]
fn diff_snapshot_maps_old_and_new_sides() {
    let diff = FileDiff {
        path: "a.rs".into(),
        old_path: None,
        change_kind: FileChangeKind::Modified,
        binary: false,
        old_mode: None,
        new_mode: None,
        hunks: vec![Hunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            heading: None,
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Delete,
                    old_lineno: Some(1),
                    new_lineno: None,
                    text: "let old = 1;".into(),
                },
                DiffLine {
                    kind: DiffLineKind::Add,
                    old_lineno: None,
                    new_lineno: Some(1),
                    text: "let new = 2;".into(),
                },
            ],
        }],
    };
    let snapshot = DiffSyntaxSnapshot::new(&diff, Some("rust"));
    let theme = HighlightTheme::default_dark();
    let key = highlight_theme_key(&theme);

    let old = snapshot.side_line(0, 0, true, &theme, key);
    let new = snapshot.side_line(0, 1, false, &theme, key);
    assert_eq!(
        old.as_ref().map(|line| line.text.as_ref()),
        Some("let old = 1;")
    );
    assert_eq!(
        new.as_ref().map(|line| line.text.as_ref()),
        Some("let new = 2;")
    );
    assert!(snapshot.side_line(0, 0, false, &theme, key).is_none());
}
