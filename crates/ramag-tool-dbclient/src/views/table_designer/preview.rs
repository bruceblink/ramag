use super::*;

pub(super) fn render_ddl_panel(
    loading: bool,
    ddl: Option<String>,
    error: Option<String>,
    scroll: &ScrollHandle,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let content = if loading {
        v_flex()
            .w_full()
            .h(px(TABLE_DDL_PANEL_HEIGHT))
            .items_center()
            .justify_center()
            .gap_2()
            .child(Spinner::new().small())
            .child(div().text_sm().child("正在加载建表语句…"))
            .into_any_element()
    } else if let Some(error) = error {
        v_flex()
            .w_full()
            .h(px(TABLE_DDL_PANEL_HEIGHT))
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_sm().child("建表语句加载失败"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(error),
            )
            .into_any_element()
    } else {
        let ddl = ddl.unwrap_or_else(|| "暂无建表语句".into());
        let highlighted_ddl = highlight_sql(ddl, &theme.highlight_theme);
        div()
            .w_full()
            .h(px(TABLE_DDL_PANEL_HEIGHT))
            .id("table-designer-ddl-scroll")
            .overflow_y_scroll()
            .track_scroll(scroll)
            .p_3()
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .whitespace_normal()
            .child(highlighted_ddl)
            .into_any_element()
    };
    div()
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded_lg()
        .bg(theme.background)
        .overflow_hidden()
        .child(content)
}

pub(super) fn highlight_sql(sql: String, theme: &HighlightTheme) -> StyledText {
    let mut highlighter = SyntaxHighlighter::new("sql");
    highlighter.update(None, &Rope::from_str(&sql), None);
    let highlights = highlighter.styles(&(0..sql.len()), theme);
    StyledText::new(sql).with_highlights(highlights)
}
pub(super) fn syntax_color(syntax: &SyntaxColors, name: &str, fallback: Hsla) -> Hsla {
    syntax
        .style(name)
        .and_then(|style| style.color)
        .unwrap_or(fallback)
}

pub(super) fn default_value_color(
    value: &str,
    keyword: Hsla,
    number: Hsla,
    string: Hsla,
    constant: Hsla,
) -> Hsla {
    let value = value.trim();
    if value.is_empty() {
        return constant;
    }
    if value.parse::<f64>().is_ok() {
        return number;
    }
    if (value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('"') && value.ends_with('"'))
    {
        return string;
    }
    if matches!(
        value.to_ascii_uppercase().as_str(),
        "NULL" | "TRUE" | "FALSE" | "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP"
    ) || value.to_ascii_uppercase().starts_with("CURRENT_TIMESTAMP(")
    {
        return keyword;
    }
    constant
}
