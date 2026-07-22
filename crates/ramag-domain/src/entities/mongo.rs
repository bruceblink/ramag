//! MongoDB 文档数据库实体。文档以 serde_json::Value 表达，
//! infra 层负责 BSON ↔ Extended JSON 双向映射（ObjectId → `{"$oid":...}` 等）

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{DomainError, Result};

/// MongoDB 数据库名服务端限制为少于 64 bytes。
pub const MAX_MONGO_DATABASE_NAME_BYTES: usize = 63;
/// 集合名与数据库名组成 namespace；单段先限制为 255 bytes，完整 namespace 由驱动再校验。
pub const MAX_MONGO_COLLECTION_NAME_BYTES: usize = 255;
/// 结果区 `$set` 使用 dotted path；限制异常路径在日志、弹窗和命令文档中的放大成本。
pub const MAX_MONGO_FIELD_PATH_BYTES: usize = 1024;
/// MongoDB 单 BSON 文档的协议上限为 16 MiB；领域层先按动态 JSON 内容做前置预算。
pub const MAX_MONGO_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_MONGO_VALUE_NODES: usize = 100_000;
pub const MAX_MONGO_NESTING_DEPTH: usize = 100;
pub const MAX_MONGO_PIPELINE_STAGES: usize = 1_000;

/// 估算单个 Extended JSON 值的常驻内存，包含容器预留空间与字符串容量。
pub fn mongo_value_retained_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(mongo_value_dynamic_bytes(value))
}

/// 估算一组 MongoDB 文档的常驻内存，供结果与传输批次预算使用。
pub fn mongo_documents_retained_bytes(documents: &[Value], capacity: usize) -> usize {
    let mut bytes = capacity.saturating_mul(std::mem::size_of::<Value>());
    for document in documents {
        bytes = bytes.saturating_add(mongo_value_dynamic_bytes(document));
    }
    bytes
}

fn mongo_value_dynamic_bytes(root: &Value) -> usize {
    let mut total = 0usize;
    let mut stack = vec![root];
    while let Some(value) = stack.pop() {
        match value {
            Value::String(text) => total = total.saturating_add(text.capacity()),
            Value::Array(items) => {
                total = total.saturating_add(
                    items
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Value>()),
                );
                stack.extend(items);
            }
            Value::Object(fields) => {
                // serde_json::Map 的节点布局不公开，额外计三个指针作为保守开销。
                let entry_bytes = std::mem::size_of::<(String, Value)>()
                    .saturating_add(3 * std::mem::size_of::<usize>());
                total = total.saturating_add(fields.len().saturating_mul(entry_bytes));
                for (key, child) in fields {
                    total = total.saturating_add(key.capacity());
                    stack.push(child);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    total
}

pub fn validate_mongo_database_name(name: &str) -> Result<()> {
    validate_mongo_name("MongoDB 数据库名", name, MAX_MONGO_DATABASE_NAME_BYTES)
}

pub fn validate_mongo_collection_name(name: &str) -> Result<()> {
    validate_mongo_name("MongoDB 集合名", name, MAX_MONGO_COLLECTION_NAME_BYTES)
}

pub fn validate_mongo_field_path(path: &str) -> Result<()> {
    validate_mongo_name("MongoDB 字段路径", path, MAX_MONGO_FIELD_PATH_BYTES)
}

fn validate_mongo_name(label: &str, name: &str, max_bytes: usize) -> Result<()> {
    if name.is_empty() {
        return Err(DomainError::InvalidConfig(format!("{label}不能为空")));
    }
    if name.len() > max_bytes {
        return Err(DomainError::InvalidConfig(format!(
            "{label}超过 {max_bytes} bytes 上限"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(DomainError::InvalidConfig(format!(
            "{label}不能包含控制字符"
        )));
    }
    Ok(())
}

pub fn validate_mongo_document(value: &Value, label: &str) -> Result<()> {
    if !value.is_object() {
        return Err(DomainError::InvalidConfig(format!(
            "{label}必须是 JSON 对象"
        )));
    }
    validate_mongo_values([value], label, MongoValueLimits::production())
}

pub fn validate_mongo_pipeline(pipeline: &[MongoDocument]) -> Result<()> {
    if pipeline.len() > MAX_MONGO_PIPELINE_STAGES {
        return Err(DomainError::InvalidConfig(format!(
            "MongoDB pipeline 超过 {MAX_MONGO_PIPELINE_STAGES} 个 stage 上限"
        )));
    }
    if pipeline.iter().any(|stage| !stage.is_object()) {
        return Err(DomainError::InvalidConfig(
            "MongoDB pipeline 的每个 stage 都必须是 JSON 对象".into(),
        ));
    }
    validate_mongo_values(
        pipeline.iter(),
        "MongoDB pipeline",
        MongoValueLimits::production(),
    )
}

#[derive(Clone, Copy)]
struct MongoValueLimits {
    bytes: usize,
    nodes: usize,
    depth: usize,
}

impl MongoValueLimits {
    const fn production() -> Self {
        Self {
            bytes: MAX_MONGO_DOCUMENT_BYTES,
            nodes: MAX_MONGO_VALUE_NODES,
            depth: MAX_MONGO_NESTING_DEPTH,
        }
    }
}

fn validate_mongo_values<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    label: &str,
    limits: MongoValueLimits,
) -> Result<()> {
    let mut stack: Vec<(&Value, usize)> = values.into_iter().map(|value| (value, 0)).collect();
    let mut nodes = 0usize;
    let mut bytes = 0usize;

    while let Some((value, depth)) = stack.pop() {
        if depth > limits.depth {
            return Err(DomainError::InvalidConfig(format!(
                "{label}嵌套超过 {} 层上限",
                limits.depth
            )));
        }
        nodes = nodes
            .checked_add(1)
            .ok_or_else(|| DomainError::InvalidConfig(format!("{label}节点数量溢出")))?;
        if nodes > limits.nodes {
            return Err(DomainError::InvalidConfig(format!(
                "{label}超过 {} 个节点上限",
                limits.nodes
            )));
        }

        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    if key.contains('\0') {
                        return Err(DomainError::InvalidConfig(format!(
                            "{label}字段名不能包含 NUL 字符"
                        )));
                    }
                    bytes = reserve_mongo_bytes(bytes, key.len(), label, limits.bytes)?;
                    stack.push((child, depth.saturating_add(1)));
                }
            }
            Value::Array(items) => {
                stack.extend(items.iter().map(|child| (child, depth.saturating_add(1))));
            }
            Value::String(text) => {
                bytes = reserve_mongo_bytes(bytes, text.len(), label, limits.bytes)?;
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    Ok(())
}

fn reserve_mongo_bytes(current: usize, added: usize, label: &str, limit: usize) -> Result<usize> {
    let next = current
        .checked_add(added)
        .ok_or_else(|| DomainError::InvalidConfig(format!("{label}动态内容长度溢出")))?;
    if next > limit {
        return Err(DomainError::InvalidConfig(format!(
            "{label}动态内容超过 {} MiB 上限",
            limit / 1024 / 1024
        )));
    }
    Ok(next)
}

/// 数据库
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongoDatabase {
    pub name: String,
    /// listDatabases 给出的字节数；admin 库或受限场景可能为 None
    pub size_on_disk: Option<u64>,
    pub empty: bool,
}

/// 集合（含 view 兼容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongoCollection {
    pub name: String,
    pub database: String,
    /// 视图，无法写入
    pub is_view: bool,
}

/// 索引。`keys` 保留 spec 顺序（复合索引语义敏感）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongoIndex {
    pub name: String,
    /// (字段名, 方向)；方向 1=升序 / -1=降序 / 文本索引等扩展为 0
    pub keys: Vec<(String, i32)>,
    pub unique: bool,
    /// `_id` 索引视为主键
    pub primary: bool,
    pub sparse: bool,
}

/// MongoDB 文档。Extended JSON 风格，
/// `ObjectId → {"$oid": "..."}`、`Decimal128 → {"$numberDecimal": "..."}`、
/// `DateTime → {"$date": "ISO8601"}`、`Binary → {"$binary": {"base64": "...", "subType": "..."}}`
pub type MongoDocument = Value;

/// `find` 查询规格
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MongoQuerySpec {
    /// filter（必须为 JSON 对象，空对象 = 匹配全部）
    pub filter: Value,
    /// 投影，None = 全部字段
    pub projection: Option<Value>,
    /// 排序 spec，例 `{"createdAt": -1}`
    pub sort: Option<Value>,
    /// 跳过文档数（分页）
    pub skip: Option<u64>,
    /// 返回上限。None 表示不限制条数，仍受调用方结果字节预算约束。
    pub limit: Option<i64>,
    /// 调用方单页内存预算；None 使用交互结果的 256 MiB 上限。
    #[serde(default)]
    pub result_byte_limit: Option<usize>,
}

impl MongoQuerySpec {
    pub fn validate(&self) -> Result<()> {
        if !self.filter.is_null() && !self.filter.is_object() {
            return Err(DomainError::InvalidConfig(
                "MongoDB filter 必须是 JSON 对象或 null".into(),
            ));
        }
        for (label, value) in [
            ("MongoDB projection", self.projection.as_ref()),
            ("MongoDB sort", self.sort.as_ref()),
        ] {
            if value.is_some_and(|value| !value.is_object()) {
                return Err(DomainError::InvalidConfig(format!(
                    "{label}必须是 JSON 对象"
                )));
            }
        }
        if self.limit == Some(i64::MIN) {
            return Err(DomainError::InvalidConfig(
                "MongoDB limit 不能是 i64::MIN".into(),
            ));
        }
        if self.result_byte_limit == Some(0) {
            return Err(DomainError::InvalidConfig(
                "MongoDB 结果字节预算必须大于 0".into(),
            ));
        }

        validate_mongo_values(
            std::iter::once(&self.filter)
                .chain(self.projection.iter())
                .chain(self.sort.iter()),
            "MongoDB 查询参数",
            MongoValueLimits::production(),
        )
    }
}

/// 查询结果。无论 read / write 都用同一结构上报
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MongoQueryResult {
    /// read 类返回的文档；write 类为空
    pub documents: Vec<MongoDocument>,
    /// write 类返回的影响数（matched / modified / deleted / inserted）
    pub affected: u64,
    pub elapsed_ms: u64,
    /// UI 状态栏 / 历史摘要，如 "12 docs, 18ms"
    pub summary: String,
    /// 结果是否被安全上限截断（游标超过上限只取前 N）。UI 据此提示"仅显示前 N 条"，
    /// 导出也据此告知用户导出的是已加载数据而非完整查询结果
    #[serde(default)]
    pub truncated: bool,
    /// 文档在客户端的常驻内存估算。
    #[serde(default)]
    pub retained_bytes: usize,
    /// 单个结果已达到 128 MiB 提示线。
    #[serde(default)]
    pub memory_warning: bool,
}

/// `insert_many` 结果：插入数与重复 `_id` 跳过数
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct InsertManyOutcome {
    pub inserted: u64,
    /// 重复 `_id`（E11000）跳过数，仅 skip_duplicates=true 时可能非 0
    pub duplicates: u64,
}

/// 集合统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongoCollectionStats {
    pub count: u64,
    pub size_bytes: u64,
    pub avg_obj_size: u64,
    pub storage_size: u64,
    pub index_count: u32,
}

impl MongoQueryResult {
    /// read 类构造（拼摘要）
    pub fn read(documents: Vec<MongoDocument>, elapsed_ms: u64) -> Self {
        Self::read_maybe_truncated(documents, elapsed_ms, false)
    }

    /// read 类构造，携带截断标志（游标超上限只取前 N 时 truncated=true）
    pub fn read_maybe_truncated(
        documents: Vec<MongoDocument>,
        elapsed_ms: u64,
        truncated: bool,
    ) -> Self {
        let retained_bytes = mongo_documents_retained_bytes(&documents, documents.capacity());
        Self::read_with_budget(documents, elapsed_ms, truncated, retained_bytes)
    }

    /// read 类构造，复用驱动流式累计出的内存估算。
    pub fn read_with_budget(
        documents: Vec<MongoDocument>,
        elapsed_ms: u64,
        truncated: bool,
        retained_bytes: usize,
    ) -> Self {
        let n = documents.len();
        let summary = if truncated {
            format!("已加载前 {n} 条（结果被截断）, {elapsed_ms}ms")
        } else {
            format!("{n} docs, {elapsed_ms}ms")
        };
        Self {
            documents,
            affected: 0,
            elapsed_ms,
            summary,
            truncated,
            retained_bytes,
            memory_warning: retained_bytes >= super::INTERACTIVE_RESULT_WARNING_BYTES,
        }
    }

    /// write 类构造
    pub fn write(affected: u64, elapsed_ms: u64, op: &str) -> Self {
        Self {
            documents: Vec::new(),
            affected,
            elapsed_ms,
            summary: format!("{op} affected={affected}, {elapsed_ms}ms"),
            truncated: false,
            retained_bytes: 0,
            memory_warning: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_summary_includes_count() {
        let r = MongoQueryResult::read(vec![json!({"a": 1}), json!({"a": 2})], 5);
        assert_eq!(r.documents.len(), 2);
        assert_eq!(r.affected, 0);
        assert!(r.summary.contains("2 docs"));
    }

    #[test]
    fn write_summary_includes_op() {
        let r = MongoQueryResult::write(3, 12, "updateOne");
        assert!(r.summary.contains("updateOne"));
        assert!(r.summary.contains("affected=3"));
    }

    #[test]
    fn mongo_names_and_query_spec_have_explicit_boundaries() {
        assert!(validate_mongo_database_name(&"d".repeat(MAX_MONGO_DATABASE_NAME_BYTES)).is_ok());
        assert!(
            validate_mongo_database_name(&"d".repeat(MAX_MONGO_DATABASE_NAME_BYTES + 1)).is_err()
        );
        assert!(validate_mongo_collection_name("users").is_ok());
        assert!(validate_mongo_collection_name("line\nitems").is_err());
        assert!(validate_mongo_field_path("profile.name").is_ok());

        let valid = MongoQuerySpec {
            filter: json!({"active": true}),
            projection: Some(json!({"name": 1})),
            sort: Some(json!({"created_at": -1})),
            skip: Some(0),
            limit: Some(100),
            result_byte_limit: None,
        };
        assert!(valid.validate().is_ok());

        let invalid = MongoQuerySpec {
            filter: json!([]),
            ..MongoQuerySpec::default()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn mongo_value_budget_bounds_bytes_nodes_depth_and_pipeline_stages() {
        let limits = MongoValueLimits {
            bytes: 3,
            nodes: 4,
            depth: 2,
        };
        assert!(validate_mongo_values([&json!({"a": "bc"})], "test", limits).is_ok());
        assert!(validate_mongo_values([&json!({"a": "bcd"})], "test", limits).is_err());
        assert!(validate_mongo_values([&json!([1, 2, 3, 4])], "test", limits).is_err());
        assert!(validate_mongo_values([&json!([[[1]]])], "test", limits).is_err());

        assert!(validate_mongo_document(&json!({"a": 1}), "文档").is_ok());
        assert!(validate_mongo_document(&json!([1]), "文档").is_err());
        assert!(validate_mongo_document(&json!({"bad\0key": 1}), "文档").is_err());

        let too_many = vec![json!({"$match": {}}); MAX_MONGO_PIPELINE_STAGES + 1];
        assert!(validate_mongo_pipeline(&too_many).is_err());
    }
}
