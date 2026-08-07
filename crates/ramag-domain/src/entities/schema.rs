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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub data_type: ColumnType,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub is_primary_key: bool,
    pub comment: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    /// 与 `columns` 一一对应。
    pub ref_columns: Vec<String>,
}
