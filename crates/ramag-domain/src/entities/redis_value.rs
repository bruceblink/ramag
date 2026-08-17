//! Redis 运行时值；ZSet 分数为 `f64`，因此不实现 `Eq` 和 `Hash`。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RedisValue {
    Nil,
    Text(String),
    /// 非 UTF-8 字符串或二进制数据。
    Bytes(Vec<u8>),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<RedisValue>),
    /// 使用 `Vec` 保留服务端顺序。
    Hash(Vec<(String, RedisValue)>),
    Set(Vec<RedisValue>),
    /// `(member, score)`，按分数升序。
    ZSet(Vec<(RedisValue, f64)>),
    Stream(Vec<StreamEntry>),
    Array(Vec<RedisValue>),
}

/// 受控加载结果；`total` 用于区分完整值和安全前缀。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisValueLoad {
    pub value: RedisValue,
    /// String 为服务端字节数；List/Hash/Set/ZSet/Stream 为服务端当前元素总数。
    pub total: Option<u64>,
    /// 内容因累计字节预算只保留了安全前缀；调用方不得把它当作完整值覆盖回服务端。
    #[serde(default)]
    pub byte_limited: bool,
    #[serde(default)]
    pub memory_warning: bool,
}

impl RedisValueLoad {
    pub fn loaded_len(&self) -> Option<usize> {
        self.value.len().or_else(|| self.value.scalar_byte_len())
    }

    pub fn has_more(&self) -> bool {
        match (self.loaded_len(), self.total) {
            (Some(loaded), Some(total)) => (loaded as u64) < total,
            _ => false,
        }
    }
}

/// 从 `Start` 起逐页读取完整值的游标。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ValuePageCursor {
    Start,
    /// List 元素偏移 / String 字节偏移
    Offset(u64),
    /// Hash / Set / ZSet 的 SCAN 族游标（0 表示读完，driver 转为 next=None 不回传）
    Scan(u64),
    /// Stream：上一页最后一条 entry id，续读用开区间
    AfterId(String),
}

#[derive(Debug, Clone)]
pub struct RedisValuePage {
    /// 与完整值使用相同 variant 的当前片段。
    pub items: RedisValue,
    /// `None` 表示读取完成。
    pub next: Option<ValuePageCursor>,
    /// 实体无法表达而跳过的条目数（如二进制 hash field 名）
    pub skipped: u64,
    /// 仅首页自动探测时返回：PTTL 毫秒（-1 永久 / -2 key 不存在）；续读页为 None
    pub ttl_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(String, String)>,
}

impl RedisValue {
    pub fn is_nil(&self) -> bool {
        matches!(self, RedisValue::Nil)
    }

    pub fn len(&self) -> Option<usize> {
        match self {
            RedisValue::List(v) | RedisValue::Set(v) | RedisValue::Array(v) => Some(v.len()),
            RedisValue::Hash(v) => Some(v.len()),
            RedisValue::ZSet(v) => Some(v.len()),
            RedisValue::Stream(v) => Some(v.len()),
            _ => None,
        }
    }

    pub fn scalar_byte_len(&self) -> Option<usize> {
        match self {
            RedisValue::Text(value) => Some(value.len()),
            RedisValue::Bytes(value) => Some(value.len()),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len().is_some_and(|n| n == 0)
    }

    pub fn display_preview(&self, max_len: usize) -> String {
        match self {
            RedisValue::Nil => "(nil)".to_string(),
            RedisValue::Text(s) => sanitize_inline(&truncate(s, max_len)),
            RedisValue::Bytes(b) => format!("[{} bytes]", b.len()),
            RedisValue::Int(i) => i.to_string(),
            RedisValue::Float(f) => f.to_string(),
            RedisValue::Bool(b) => b.to_string(),
            RedisValue::List(v) => format!("List({} elems)", v.len()),
            RedisValue::Hash(v) => format!("Hash({} fields)", v.len()),
            RedisValue::Set(v) => format!("Set({} elems)", v.len()),
            RedisValue::ZSet(v) => format!("ZSet({} elems)", v.len()),
            RedisValue::Stream(v) => format!("Stream({} entries)", v.len()),
            RedisValue::Array(v) => format!("Array({} elems)", v.len()),
        }
    }

    /// 生成复制用的完整值；集合 / 数组不会退化成 `List(N elems)` 等预览摘要。
    ///
    /// 根标量保留 Redis 客户端常见的原始文本语义；复合值使用 JSON 形态，二进制叶子
    /// 用 `$bytes` 包装并编码为十六进制，保证不同数据类型嵌套时仍然不会丢失类型边界。
    pub fn to_clipboard_string(&self) -> String {
        match self {
            Self::Nil => String::new(),
            Self::Text(value) => value.clone(),
            Self::Bytes(value) => bytes_to_hex(value),
            Self::Int(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::List(_)
            | Self::Hash(_)
            | Self::Set(_)
            | Self::ZSet(_)
            | Self::Stream(_)
            | Self::Array(_) => serde_json::to_string_pretty(&to_json_value(self))
                .unwrap_or_else(|_| self.display_preview(256)),
        }
    }
}

fn to_json_value(value: &RedisValue) -> serde_json::Value {
    use serde_json::{Map, Value, json};

    match value {
        RedisValue::Nil => Value::Null,
        RedisValue::Text(value) => Value::String(value.clone()),
        RedisValue::Bytes(value) => json!({"$bytes": bytes_to_hex(value)}),
        RedisValue::Int(value) => Value::Number((*value).into()),
        RedisValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(value.to_string())),
        RedisValue::Bool(value) => Value::Bool(*value),
        RedisValue::List(values) | RedisValue::Set(values) | RedisValue::Array(values) => {
            Value::Array(values.iter().map(to_json_value).collect())
        }
        RedisValue::Hash(values) => {
            let mut object = Map::new();
            for (field, value) in values {
                object.insert(field.clone(), to_json_value(value));
            }
            Value::Object(object)
        }
        RedisValue::ZSet(values) => Value::Array(
            values
                .iter()
                .map(|(member, score)| {
                    json!({
                        "member": to_json_value(member),
                        "score": serde_json::Number::from_f64(*score)
                            .map(Value::Number)
                            .unwrap_or_else(|| Value::String(score.to_string())),
                    })
                })
                .collect(),
        ),
        RedisValue::Stream(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| {
                    let fields = entry
                        .fields
                        .iter()
                        .map(|(field, value)| (field.clone(), Value::String(value.clone())))
                        .collect();
                    json!({"id": entry.id, "fields": Value::Object(fields)})
                })
                .collect(),
        ),
    }
}

fn bytes_to_hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn truncate(s: &str, max_len: usize) -> String {
    let mut chars = s.chars();
    let mut preview: String = chars.by_ref().take(max_len).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

/// GPUI 单行文本不接受换行符。
fn sanitize_inline(s: &str) -> String {
    if s.contains(['\n', '\r']) {
        s.replace(['\n', '\r'], " ")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nil_is_nil() {
        assert!(RedisValue::Nil.is_nil());
        assert!(!RedisValue::Int(0).is_nil());
    }

    #[test]
    fn len_for_composites() {
        assert_eq!(RedisValue::Text("a".into()).len(), None);
        assert_eq!(
            RedisValue::List(vec![RedisValue::Int(1), RedisValue::Int(2)]).len(),
            Some(2)
        );
        assert_eq!(
            RedisValue::Hash(vec![("k".into(), RedisValue::Int(1))]).len(),
            Some(1)
        );
    }

    #[test]
    fn preview_truncates_long_text() {
        let long: String = "a".repeat(100);
        let preview = RedisValue::Text(long).display_preview(10);
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= 11);
    }

    #[test]
    fn preview_bytes_shows_size() {
        let v = RedisValue::Bytes(vec![0u8; 1024]);
        assert_eq!(v.display_preview(80), "[1024 bytes]");
    }

    #[test]
    fn preview_text_strips_newlines() {
        let v = RedisValue::Text("line1\nline2\r\nline3".to_string());
        let p = v.display_preview(80);
        assert!(!p.contains('\n') && !p.contains('\r'));
    }

    #[test]
    fn clipboard_string_keeps_root_scalar_semantics() {
        assert_eq!(
            RedisValue::Text("hello".into()).to_clipboard_string(),
            "hello"
        );
        assert_eq!(
            RedisValue::Bytes(vec![0, 15, 255]).to_clipboard_string(),
            "000fff"
        );
        assert_eq!(RedisValue::Nil.to_clipboard_string(), "");
    }

    #[test]
    fn clipboard_string_preserves_nested_types() {
        let value = RedisValue::Hash(vec![
            (
                "items".into(),
                RedisValue::List(vec![
                    RedisValue::Text("alice".into()),
                    RedisValue::Bytes(vec![0xaa]),
                ]),
            ),
            (
                "scores".into(),
                RedisValue::ZSet(vec![(RedisValue::Text("alice".into()), 1.5)]),
            ),
        ]);

        let copied = value.to_clipboard_string();
        assert!(copied.contains("\"items\""));
        assert!(copied.contains("\"alice\""));
        assert!(copied.contains("\"$bytes\": \"aa\""));
        assert!(copied.contains("\"score\": 1.5"));
    }

    #[test]
    fn value_load_reports_partial_collection() {
        let load = RedisValueLoad {
            value: RedisValue::List(vec![RedisValue::Int(1)]),
            total: Some(2),
            byte_limited: false,
            memory_warning: false,
        };

        assert_eq!(load.loaded_len(), Some(1));
        assert!(load.has_more());
    }

    #[test]
    fn value_load_reports_partial_string_bytes() {
        let load = RedisValueLoad {
            value: RedisValue::Text("hello".into()),
            total: Some(10),
            byte_limited: true,
            memory_warning: false,
        };

        assert_eq!(load.loaded_len(), Some(5));
        assert!(load.has_more());
        assert!(load.byte_limited);
    }
}
