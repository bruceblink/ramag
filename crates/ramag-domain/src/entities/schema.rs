//! 数据库元数据实体。

use serde::{Deserialize, Serialize};

/// MySQL 数据库或 PostgreSQL schema。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,
    pub charset: Option<String>,
    pub collation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub schema: String,
    pub comment: Option<String>,
    /// 兼容缺少该字段的旧记录。
    #[serde(default)]
    pub is_view: bool,
    /// 物理存储大小（字节）；视图或驱动暂时无法取得统计时为空。
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub data_type: ColumnType,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub is_primary_key: bool,
    pub comment: Option<String>,
    /// 数据库中的 1-based 列序号；旧记录或不完整元数据可能没有该值。
    #[serde(default)]
    pub ordinal_position: Option<u32>,
    /// MySQL `AUTO_INCREMENT` 列标记。
    #[serde(default)]
    pub is_auto_increment: bool,
    /// 生成列表达式，不包含外围的 `AS (...)` 子句。
    #[serde(default)]
    pub generation_expression: Option<String>,
    /// 生成列的存储方式；具体驱动决定可读取和可生成的存储方式。
    #[serde(default)]
    pub generated_storage: Option<GeneratedColumnStorage>,
    /// PostgreSQL `IDENTITY` 列的生成模式。
    #[serde(default)]
    pub identity_generation: Option<IdentityGeneration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneratedColumnStorage {
    Virtual,
    Stored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityGeneration {
    Always,
    ByDefault,
}

/// `raw_type` 保留 `VARCHAR(255)` 等数据库原始类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnType {
    pub kind: ColumnKind,
    pub raw_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnKind {
    Integer,
    Decimal,
    Float,
    Text,
    Blob,
    Bool,
    DateTime,
    Json,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub primary: bool,
    /// 索引列，按顺序
    pub columns: Vec<String>,
}

/// 外键在被引用行删除或更新时采用的数据库动作。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForeignKeyAction {
    /// 数据库默认动作：不自动级联，按约束检查失败。
    #[default]
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl ForeignKeyAction {
    /// 将数据库元数据中的动作名称转换为跨驱动的规范值；未知值返回 None 以阻止信息丢失。
    pub fn parse_sql(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("NO ACTION") || value.eq_ignore_ascii_case("NO_ACTION") {
            Some(Self::NoAction)
        } else if value.eq_ignore_ascii_case("RESTRICT") {
            Some(Self::Restrict)
        } else if value.eq_ignore_ascii_case("CASCADE") {
            Some(Self::Cascade)
        } else if value.eq_ignore_ascii_case("SET NULL") || value.eq_ignore_ascii_case("SET_NULL") {
            Some(Self::SetNull)
        } else if value.eq_ignore_ascii_case("SET DEFAULT")
            || value.eq_ignore_ascii_case("SET_DEFAULT")
        {
            Some(Self::SetDefault)
        } else {
            None
        }
    }

    /// 返回可直接放入 MySQL/PostgreSQL 外键定义的规范 SQL 片段。
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::NoAction => "NO ACTION",
            Self::Restrict => "RESTRICT",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    /// 与 `columns` 一一对应。
    pub ref_columns: Vec<String>,
    /// 引用行被删除时的动作；缺少该字段的旧记录按数据库默认行为处理。
    #[serde(default)]
    pub on_delete: ForeignKeyAction,
    /// 引用键被更新时的动作；缺少该字段的旧记录按数据库默认行为处理。
    #[serde(default)]
    pub on_update: ForeignKeyAction,
}

#[cfg(test)]
mod tests {
    use super::{Column, ColumnKind, ForeignKeyAction, Table};

    #[test]
    fn table_deserializes_legacy_records_without_size() {
        let result =
            serde_json::from_str::<Table>(r#"{"name":"users","schema":"public","comment":null}"#);
        assert!(result.is_ok(), "旧表记录应继续可反序列化");
        let Some(table) = result.ok() else { return };
        assert!(!table.is_view);
        assert_eq!(table.size_bytes, None);
    }

    #[test]
    fn column_deserializes_legacy_records_without_new_metadata() {
        let result = serde_json::from_str::<Column>(
            r#"{
                "name":"id",
                "data_type":{"kind":"Integer","raw_type":"int"},
                "nullable":false,
                "default_value":null,
                "is_primary_key":true,
                "comment":null
            }"#,
        );
        assert!(result.is_ok(), "legacy column should deserialize");
        let Some(column) = result.ok() else { return };
        assert_eq!(column.name, "id");
        assert_eq!(column.data_type.kind, ColumnKind::Integer);
        assert_eq!(column.ordinal_position, None);
        assert!(!column.is_auto_increment);
        assert_eq!(column.generation_expression, None);
        assert_eq!(column.generated_storage, None);
        assert_eq!(column.identity_generation, None);
    }

    #[test]
    fn parses_sql_actions_without_losing_unknown_values() {
        assert_eq!(
            ForeignKeyAction::parse_sql(" set null "),
            Some(ForeignKeyAction::SetNull)
        );
        assert_eq!(
            ForeignKeyAction::parse_sql("NO_ACTION"),
            Some(ForeignKeyAction::NoAction)
        );
        assert_eq!(ForeignKeyAction::parse_sql("MATCH FULL"), None);
    }

    #[test]
    fn renders_canonical_sql_actions() {
        assert_eq!(ForeignKeyAction::Cascade.as_sql(), "CASCADE");
        assert_eq!(ForeignKeyAction::SetDefault.as_sql(), "SET DEFAULT");
    }
}
