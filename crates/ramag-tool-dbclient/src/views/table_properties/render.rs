//! 表属性视图的结构化只读内容渲染。

use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{Icon, IconName, Sizable as _, Theme, h_flex, v_flex};
use ramag_domain::entities::{Column, ForeignKey, Index, Trigger};

const OUTLINE_WIDTH: f32 = 216.0;

pub(super) fn is_key(index: &Index) -> bool {
    index.primary || index.unique
}

pub(super) fn render_outline(
    columns: usize,
    keys: usize,
    indexes: usize,
    foreign_keys: usize,
    triggers: usize,
    theme: &Theme,
) -> impl IntoElement {
    v_flex()
        .w(px(OUTLINE_WIDTH))
        .h_full()
        .flex_none()
        .min_h_0()
        .gap(px(2.0))
        .p(px(8.0))
        .border_1()
        .border_color(theme.border)
        .rounded(px(6.0))
        .bg(theme.secondary)
        .child(
            div()
                .px(px(6.0))
                .py(px(5.0))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child("表结构"),
        )
        .child(outline_item(
            "列",
            columns,
            IconName::MemoryStick,
            theme,
            true,
        ))
        .child(outline_item("键", keys, IconName::File, theme, true))
        .child(outline_item("索引", indexes, IconName::File, theme, true))
        .child(outline_item(
            "外键",
            foreign_keys,
            IconName::ArrowRight,
            theme,
            true,
        ))
        .child(outline_item(
            "触发器",
            triggers,
            IconName::Network,
            theme,
            true,
        ))
}

fn outline_item(
    title: &'static str,
    count: usize,
    icon: IconName,
    theme: &Theme,
    expanded: bool,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .h(px(28.0))
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .px(px(4.0))
        .rounded(px(3.0))
        .bg(if expanded {
            theme.muted.opacity(0.55)
        } else {
            theme.secondary
        })
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground),
        )
        .child(Icon::new(icon).xsmall().text_color(theme.muted_foreground))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.foreground)
                .child(title),
        )
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(count.to_string()),
        )
}

pub(super) fn render_warnings(warnings: &[String], theme: &Theme) -> AnyElement {
    v_flex()
        .w_full()
        .gap(px(2.0))
        .p(px(8.0))
        .border_1()
        .border_color(theme.warning.opacity(0.45))
        .rounded(px(5.0))
        .bg(theme.warning.opacity(0.08))
        .children(
            warnings
                .iter()
                .cloned()
                .map(|warning| div().text_xs().text_color(theme.warning).child(warning)),
        )
        .into_any_element()
}

pub(super) fn render_section(
    title: &'static str,
    count: usize,
    icon: IconName,
    body: AnyElement,
    theme: &Theme,
) -> AnyElement {
    v_flex()
        .debug_selector(|| format!("table-properties-section-{title}"))
        .w_full()
        .flex_none()
        .overflow_hidden()
        .border_1()
        .border_color(theme.border)
        .rounded(px(5.0))
        .bg(theme.secondary)
        .child(
            h_flex()
                .w_full()
                .h(px(32.0))
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .bg(theme.muted.opacity(0.45))
                .border_b_1()
                .border_color(theme.border)
                .child(
                    Icon::new(IconName::ChevronDown)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(Icon::new(icon).small().text_color(theme.accent))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                ),
        )
        .child(body)
        .into_any_element()
}

pub(super) fn render_columns(columns: &[Column], theme: &Theme) -> AnyElement {
    if columns.is_empty() {
        return empty_section("没有列元数据", theme);
    }
    let mut rows = v_flex().w_full().gap(px(1.0));
    rows = rows.child(column_header(theme));
    for (index, column) in columns.iter().enumerate() {
        let flags = column_flags(column);
        let default = column.default_value.as_deref().unwrap_or("-");
        let comment = column.comment.as_deref().unwrap_or("-");
        let ordinal = column
            .ordinal_position
            .map_or_else(|| (index + 1).to_string(), |value| value.to_string());
        rows = rows.child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(5.0))
                .border_b_1()
                .border_color(theme.border.opacity(0.45))
                .child(cell(ordinal, 42.0, theme.muted_foreground))
                .child(cell(flags + &column.name, 190.0, theme.foreground))
                .child(cell(column.data_type.raw_type.clone(), 170.0, theme.info))
                .child(cell(
                    if column.nullable { "是" } else { "否" }.to_owned(),
                    54.0,
                    theme.muted_foreground,
                ))
                .child(cell(default.to_owned(), 210.0, theme.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(comment.to_owned()),
                ),
        );
    }
    rows.into_any_element()
}

fn column_header(theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(5.0))
        .bg(theme.muted.opacity(0.3))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(cell("#".to_owned(), 42.0, theme.muted_foreground))
        .child(cell("名称".to_owned(), 190.0, theme.muted_foreground))
        .child(cell("类型".to_owned(), 170.0, theme.muted_foreground))
        .child(cell("可空".to_owned(), 54.0, theme.muted_foreground))
        .child(cell("默认值".to_owned(), 210.0, theme.muted_foreground))
        .child(div().flex_1().min_w_0().child("注释"))
}

fn column_flags(column: &Column) -> String {
    let mut flags = String::new();
    if column.is_primary_key {
        flags.push_str("PK ");
    }
    if column.is_auto_increment {
        flags.push_str("AI ");
    }
    if column.generation_expression.is_some() {
        flags.push_str("GEN ");
    }
    flags
}

fn cell(text: String, width: f32, color: gpui::Hsla) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_none()
        .min_w_0()
        .text_xs()
        .text_color(color)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(text)
}

pub(super) fn render_indexes(indexes: &[&Index], theme: &Theme, keys: bool) -> AnyElement {
    if indexes.is_empty() {
        return empty_section(
            if keys {
                "没有键约束"
            } else {
                "没有普通索引"
            },
            theme,
        );
    }
    let mut rows = v_flex().w_full();
    for index in indexes {
        let kind = if index.primary {
            "PRIMARY KEY"
        } else if index.unique {
            "UNIQUE KEY"
        } else {
            "INDEX"
        };
        rows = rows.child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(7.0))
                .border_b_1()
                .border_color(theme.border.opacity(0.45))
                .child(Icon::new(IconName::File).small().text_color(theme.warning))
                .child(cell(index.name.clone(), 220.0, theme.foreground))
                .child(cell(kind.to_owned(), 130.0, theme.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.info)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(index.columns.join(", ")),
                ),
        );
    }
    rows.into_any_element()
}

pub(super) fn render_foreign_keys(keys: &[ForeignKey], theme: &Theme) -> AnyElement {
    if keys.is_empty() {
        return empty_section("没有外键约束", theme);
    }
    let mut rows = v_flex().w_full();
    for key in keys {
        rows = rows.child(
            v_flex()
                .w_full()
                .gap(px(3.0))
                .px(px(10.0))
                .py(px(7.0))
                .border_b_1()
                .border_color(theme.border.opacity(0.45))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            Icon::new(IconName::ArrowRight)
                                .small()
                                .text_color(theme.warning),
                        )
                        .child(cell(key.name.clone(), 220.0, theme.foreground))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme.info)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(format!(
                                    "{} -> {}.{} ({})",
                                    key.columns.join(", "),
                                    key.ref_schema,
                                    key.ref_table,
                                    key.ref_columns.join(", ")
                                )),
                        ),
                )
                .child(
                    div()
                        .pl(px(28.0))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "ON DELETE {} · ON UPDATE {}",
                            key.on_delete.as_sql(),
                            key.on_update.as_sql()
                        )),
                ),
        );
    }
    rows.into_any_element()
}

pub(super) fn render_triggers(triggers: &[Trigger], theme: &Theme) -> AnyElement {
    if triggers.is_empty() {
        return empty_section("没有触发器", theme);
    }
    let mut rows = v_flex().w_full();
    for trigger in triggers {
        rows = rows.child(
            v_flex()
                .w_full()
                .gap(px(3.0))
                .px(px(10.0))
                .py(px(7.0))
                .border_b_1()
                .border_color(theme.border.opacity(0.45))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            Icon::new(IconName::Network)
                                .small()
                                .text_color(theme.warning),
                        )
                        .child(cell(trigger.name.clone(), 220.0, theme.foreground))
                        .child(cell(trigger.timing.clone(), 120.0, theme.muted_foreground))
                        .child(cell(trigger.event.clone(), 180.0, theme.info)),
                )
                .child(
                    div()
                        .w_full()
                        .pl(px(28.0))
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .whitespace_normal()
                        .child(trigger.definition.clone()),
                ),
        );
    }
    rows.into_any_element()
}

fn empty_section(message: &'static str, theme: &Theme) -> AnyElement {
    div()
        .w_full()
        .px(px(10.0))
        .py(px(14.0))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(message)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::is_key;
    use ramag_domain::entities::Index;

    fn index(primary: bool, unique: bool) -> Index {
        Index {
            name: "idx".into(),
            primary,
            unique,
            columns: vec!["id".into()],
        }
    }

    #[test]
    fn groups_primary_and_unique_indexes_as_keys() {
        assert!(is_key(&index(true, true)));
        assert!(is_key(&index(false, true)));
        assert!(!is_key(&index(false, false)));
    }
}
