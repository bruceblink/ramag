use super::*;

/// 一行字段差异的类别，用于保持预览中的增加、删除和未变更语义清晰。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FieldDiffKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldSnapshot {
    pub(super) name: String,
    pub(super) data_type: String,
    pub(super) nullable: bool,
    pub(super) default_value: String,
    pub(super) comment: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldDiffLine {
    pub(super) kind: FieldDiffKind,
    pub(super) text: String,
}

/// 将原始字段与当前字段转换成可读的列级差异，供预览和复制共用。
pub(super) fn build_field_diff(fields: &[FieldDraft], cx: &gpui::App) -> Vec<FieldDiffLine> {
    let original = fields
        .iter()
        .filter_map(|field| field.original.as_ref().map(FieldSnapshot::from_column))
        .collect::<Vec<_>>();
    let current = fields
        .iter()
        .filter(|field| !field.deleted)
        .map(|field| FieldSnapshot {
            name: field.name.read(cx).value().trim().to_string(),
            data_type: field.data_type.read(cx).value().trim().to_string(),
            nullable: field.nullable,
            default_value: field.default_value.read(cx).value().trim().to_string(),
            comment: field.comment.read(cx).value().trim().to_string(),
        })
        .collect::<Vec<_>>();
    field_diff(&original, &current)
}

/// 以字段名作为稳定匹配键，改变的字段显示为删除后新增，避免误导执行语义。
pub(super) fn field_diff(
    original: &[FieldSnapshot],
    current: &[FieldSnapshot],
) -> Vec<FieldDiffLine> {
    let mut lines = Vec::new();
    for old in original {
        let matching = current
            .iter()
            .find(|new| new.name.eq_ignore_ascii_case(&old.name));
        match matching {
            Some(new) if new == old => lines.push(FieldDiffLine {
                kind: FieldDiffKind::Context,
                text: old.display_text(),
            }),
            Some(new) => {
                lines.push(FieldDiffLine {
                    kind: FieldDiffKind::Removed,
                    text: old.display_text(),
                });
                lines.push(FieldDiffLine {
                    kind: FieldDiffKind::Added,
                    text: new.display_text(),
                });
            }
            None => lines.push(FieldDiffLine {
                kind: FieldDiffKind::Removed,
                text: old.display_text(),
            }),
        }
    }
    for new in current {
        if !original
            .iter()
            .any(|old| old.name.eq_ignore_ascii_case(&new.name))
        {
            lines.push(FieldDiffLine {
                kind: FieldDiffKind::Added,
                text: new.display_text(),
            });
        }
    }
    lines
}

/// 生成复制到剪贴板的统一文本格式，保留差异前缀以便粘贴后继续阅读。
pub(super) fn format_field_diff(lines: &[FieldDiffLine]) -> String {
    lines
        .iter()
        .map(|line| format!("{} {}", line.kind.prefix(), line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 在现有 SQL 预览滚动区域中绘制字段级增删差异。
pub(super) fn render_field_diff_lines(
    lines: &[FieldDiffLine],
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let mut body = v_flex().w_full().gap(px(2.0));
    for line in lines {
        let (prefix, color, background) = match line.kind {
            FieldDiffKind::Context => (" ", theme.muted_foreground, theme.background),
            FieldDiffKind::Added => ("+", theme.success, theme.success.opacity(0.08)),
            FieldDiffKind::Removed => ("-", theme.danger, theme.danger.opacity(0.08)),
        };
        body = body.child(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(6.0))
                .px(px(4.0))
                .py(px(2.0))
                .rounded_sm()
                .bg(background)
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .child(div().w(px(12.0)).text_color(color).child(prefix))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(color)
                        .child(line.text.clone()),
                ),
        );
    }
    body
}

impl FieldSnapshot {
    fn from_column(column: &Column) -> Self {
        Self {
            name: column.name.clone(),
            data_type: column.data_type.raw_type.clone(),
            nullable: column.nullable,
            default_value: column.default_value.clone().unwrap_or_default(),
            comment: column.comment.clone().unwrap_or_default(),
        }
    }

    fn display_text(&self) -> String {
        let nullable = if self.nullable { "NULL" } else { "NOT NULL" };
        let default = if self.default_value.is_empty() {
            "DEFAULT -".to_string()
        } else {
            format!("DEFAULT {}", compact_diff_value(&self.default_value))
        };
        let comment = if self.comment.is_empty() {
            "COMMENT -".to_string()
        } else {
            format!("COMMENT {}", compact_diff_value(&self.comment))
        };
        format!(
            "{} | {} | {} | {} | {}",
            compact_diff_value(&self.name),
            compact_diff_value(&self.data_type),
            nullable,
            default,
            comment
        )
    }
}

impl FieldDiffKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Context => " ",
            Self::Added => "+",
            Self::Removed => "-",
        }
    }
}

fn compact_diff_value(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{FieldDiffKind, FieldSnapshot, field_diff, format_field_diff};

    fn snapshot(name: &str, data_type: &str) -> FieldSnapshot {
        FieldSnapshot {
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            default_value: String::new(),
            comment: String::new(),
        }
    }

    #[test]
    fn unchanged_fields_are_context_and_changes_are_replace_pairs() {
        let original = [snapshot("id", "int"), snapshot("name", "varchar(64)")];
        let current = [snapshot("id", "int"), snapshot("email", "text")];
        let diff = field_diff(&original, &current);

        assert_eq!(
            diff.iter().map(|line| line.kind).collect::<Vec<_>>(),
            [
                FieldDiffKind::Context,
                FieldDiffKind::Removed,
                FieldDiffKind::Added,
            ]
        );
        assert!(format_field_diff(&diff).contains("- name | varchar(64)"));
        assert!(format_field_diff(&diff).contains("+ email | text"));
    }

    #[test]
    fn matching_is_case_insensitive_and_newlines_are_compacted() {
        let original = [snapshot("UserName", "text")];
        let mut current = snapshot("username", "text");
        current.comment = "line1\nline2".into();
        let diff = field_diff(&original, &[current]);

        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].kind, FieldDiffKind::Removed);
        assert_eq!(diff[1].kind, FieldDiffKind::Added);
        assert!(diff[1].text.contains("COMMENT line1 line2"));
    }
}
