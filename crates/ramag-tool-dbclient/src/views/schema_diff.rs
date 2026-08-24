//! SQL 表结构对比的纯元数据模型与差异计算。

use ramag_domain::entities::{Column, ForeignKey, Index};

#[derive(Clone, Debug, Default)]
pub(crate) struct TableMetadata {
    pub(crate) columns: Vec<Column>,
    pub(crate) indexes: Vec<Index>,
    pub(crate) foreign_keys: Vec<ForeignKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetadataDiffKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MetadataDiffLine {
    pub(crate) kind: MetadataDiffKind,
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MetadataDiffSection {
    pub(crate) title: &'static str,
    pub(crate) lines: Vec<MetadataDiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NamedMetadata {
    key: String,
    text: String,
}

/// 按对象名称比较两张表，修改项表示为删除旧定义后新增新定义。
pub(crate) fn build_table_diff(
    source: &TableMetadata,
    target: &TableMetadata,
) -> Vec<MetadataDiffSection> {
    vec![
        MetadataDiffSection {
            title: "列",
            lines: diff_named(
                source.columns.iter().map(column_entry),
                target.columns.iter().map(column_entry),
            ),
        },
        MetadataDiffSection {
            title: "索引",
            lines: diff_named(
                source.indexes.iter().map(index_entry),
                target.indexes.iter().map(index_entry),
            ),
        },
        MetadataDiffSection {
            title: "外键",
            lines: diff_named(
                source.foreign_keys.iter().map(foreign_key_entry),
                target.foreign_keys.iter().map(foreign_key_entry),
            ),
        },
    ]
}

/// 生成可复制的统一差异文本，保留对象分组和 +/- 前缀。
pub(crate) fn format_table_diff(sections: &[MetadataDiffSection]) -> String {
    let mut output = String::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(section.title);
        output.push('\n');
        if section.lines.is_empty() {
            output.push_str("  （无对象）\n");
            continue;
        }
        for line in &section.lines {
            output.push_str(line.kind.prefix());
            output.push(' ');
            output.push_str(&line.text);
            output.push('\n');
        }
    }
    output.trim_end_matches('\n').to_string()
}

fn diff_named(
    source: impl IntoIterator<Item = NamedMetadata>,
    target: impl IntoIterator<Item = NamedMetadata>,
) -> Vec<MetadataDiffLine> {
    let source: Vec<_> = source.into_iter().collect();
    let target: Vec<_> = target.into_iter().collect();
    let mut lines = Vec::new();

    for old in &source {
        match target.iter().find(|new| new.key == old.key) {
            Some(new) if new.text == old.text => lines.push(MetadataDiffLine {
                kind: MetadataDiffKind::Context,
                text: old.text.clone(),
            }),
            Some(new) => {
                lines.push(MetadataDiffLine {
                    kind: MetadataDiffKind::Removed,
                    text: old.text.clone(),
                });
                lines.push(MetadataDiffLine {
                    kind: MetadataDiffKind::Added,
                    text: new.text.clone(),
                });
            }
            None => lines.push(MetadataDiffLine {
                kind: MetadataDiffKind::Removed,
                text: old.text.clone(),
            }),
        }
    }
    for new in &target {
        if !source.iter().any(|old| old.key == new.key) {
            lines.push(MetadataDiffLine {
                kind: MetadataDiffKind::Added,
                text: new.text.clone(),
            });
        }
    }
    lines
}

fn column_entry(column: &Column) -> NamedMetadata {
    let default = column
        .default_value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(compact)
        .unwrap_or_else(|| "-".into());
    let comment = column
        .comment
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(compact)
        .unwrap_or_else(|| "-".into());
    let nullable = if column.nullable { "NULL" } else { "NOT NULL" };
    let primary = if column.is_primary_key {
        " PRIMARY KEY"
    } else {
        ""
    };
    NamedMetadata {
        key: column.name.to_ascii_lowercase(),
        text: format!(
            "{} | {} | {}{} | DEFAULT {} | COMMENT {}",
            compact(&column.name),
            compact(&column.data_type.raw_type),
            nullable,
            primary,
            default,
            comment
        ),
    }
}

fn index_entry(index: &Index) -> NamedMetadata {
    let kind = if index.primary {
        "PRIMARY"
    } else if index.unique {
        "UNIQUE"
    } else {
        "INDEX"
    };
    NamedMetadata {
        key: index.name.to_ascii_lowercase(),
        text: format!(
            "{} {} ({})",
            kind,
            compact(&index.name),
            index
                .columns
                .iter()
                .map(|column| compact(column))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn foreign_key_entry(foreign_key: &ForeignKey) -> NamedMetadata {
    NamedMetadata {
        key: foreign_key.name.to_ascii_lowercase(),
        text: format!(
            "{} ({}) -> {}.{} ({})",
            compact(&foreign_key.name),
            foreign_key
                .columns
                .iter()
                .map(|column| compact(column))
                .collect::<Vec<_>>()
                .join(", "),
            compact(&foreign_key.ref_schema),
            compact(&foreign_key.ref_table),
            foreign_key
                .ref_columns
                .iter()
                .map(|column| compact(column))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

impl MetadataDiffKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Context => " ",
            Self::Added => "+",
            Self::Removed => "-",
        }
    }
}

fn compact(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{ColumnKind, ColumnType};

    fn column(name: &str, raw_type: &str) -> Column {
        Column {
            name: name.into(),
            data_type: ColumnType {
                kind: ColumnKind::Other,
                raw_type: raw_type.into(),
            },
            nullable: true,
            default_value: None,
            is_primary_key: false,
            comment: None,
        }
    }

    fn index(name: &str, columns: &[&str]) -> Index {
        Index {
            name: name.into(),
            unique: false,
            primary: false,
            columns: columns.iter().map(|column| (*column).into()).collect(),
        }
    }

    #[test]
    fn compares_columns_indexes_and_foreign_keys_in_separate_sections() {
        let source = TableMetadata {
            columns: vec![column("id", "int")],
            indexes: vec![index("idx_old", &["id"])],
            foreign_keys: vec![],
        };
        let target = TableMetadata {
            columns: vec![column("id", "bigint"), column("email", "text")],
            indexes: vec![index("idx_new", &["email"])],
            foreign_keys: vec![],
        };

        let sections = build_table_diff(&source, &target);
        assert_eq!(
            sections
                .iter()
                .map(|section| section.title)
                .collect::<Vec<_>>(),
            ["列", "索引", "外键"]
        );
        assert_eq!(sections[0].lines.len(), 3);
        assert_eq!(sections[1].lines.len(), 2);
        assert!(format_table_diff(&sections).contains("+ email | text"));
        assert!(format_table_diff(&sections).contains("- INDEX idx_old"));
    }

    #[test]
    fn matching_is_case_insensitive_and_copy_text_keeps_sections() {
        let source = TableMetadata {
            columns: vec![column("UserName", "text")],
            ..TableMetadata::default()
        };
        let target = TableMetadata {
            columns: vec![column("username", "text")],
            ..TableMetadata::default()
        };

        let sections = build_table_diff(&source, &target);
        assert_eq!(sections[0].lines[0].kind, MetadataDiffKind::Removed);
        assert_eq!(sections[0].lines[1].kind, MetadataDiffKind::Added);
        let text = format_table_diff(&sections);
        assert!(text.contains("- UserName | text"));
        assert!(text.contains("+ username | text"));
        assert!(text.contains("索引\n  （无对象）"));
    }
}
