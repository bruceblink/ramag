//! Redis 运行时值。不实现 Eq/Hash（内部含 f64 ZSet score）

use serde::{Deserialize, Serialize};

/// Redis 单 key 完整值。`KvDriver::get_value` 按 TYPE 自动 dispatch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RedisValue {
    /// key 不存在 / nil bulk
    Nil,
    /// UTF-8 可解码 String
    Text(String),
    /// UTF-8 解码失败的 fallback，或 BLOB
    Bytes(Vec<u8>),
    /// INCR 应答 / String 数字编码
    Int(i64),
    /// RESP3 浮点数或 ZSCORE。
    Float(f64),
    /// RESP3 布尔值。
    Bool(bool),
    /// 保留服务端顺序
    List(Vec<RedisValue>),
    /// 用 Vec 保留 HSET 顺序
    Hash(Vec<(String, RedisValue)>),
    /// 唯一元素由服务端保证
    Set(Vec<RedisValue>),
    /// (member, score)，按 score 升序
    ZSet(Vec<(RedisValue, f64)>),
    /// 按时间序排列
    Stream(Vec<StreamEntry>),
    /// 通用数组（CONFIG GET / CLUSTER NODES 等复合应答）
    Array(Vec<RedisValue>),
}

/// Redis key 的受控加载结果。集合值按元素、String 按字节限制；`total` 保存服务端总量，
/// 让界面明确区分“已完整加载”和“当前只展示一部分”。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisValueLoad {
    pub value: RedisValue,
    /// String 为服务端字节数；List/Hash/Set/ZSet/Stream 为服务端当前元素总数。
    pub total: Option<u64>,
    /// 内容因累计字节预算只保留了安全前缀；调用方不得把它当作完整值覆盖回服务端。
    #[serde(default)]
    pub byte_limited: bool,
    /// 是否达到单结果提示线。
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

/// 导出用分段读取游标。与 `get_value_limited`（UI 受限预览）不同：
/// 配合 `KvDriver::read_value_page` 从 `Start` 起逐页读完整值，不截断
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ValuePageCursor {
    /// 首页
    Start,
    /// List 元素偏移 / String 字节偏移
    Offset(u64),
    /// Hash / Set / ZSet 的 SCAN 族游标（0 表示读完，driver 转为 next=None 不回传）
    Scan(u64),
    /// Stream：上一页最后一条 entry id，续读用开区间
    AfterId(String),
}

/// 一页完整值片段
#[derive(Debug, Clone)]
pub struct RedisValuePage {
    /// 与整值同构的片段：List → List 片段、Hash → Hash 片段、String → Text / Bytes 片段。
    /// 首页未传类型时，调用方从 variant 得知 key 类型
    pub items: RedisValue,
    /// None = 已读完
    pub next: Option<ValuePageCursor>,
    /// 实体无法表达而跳过的条目数（如二进制 hash field 名）
    pub skipped: u64,
    /// 仅首页自动探测时返回：PTTL 毫秒（-1 永久 / -2 key 不存在）；续读页为 None
    pub ttl_ms: Option<i64>,
}

/// Stream 单条消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEntry {
    /// 形如 `<ms>-<seq>`
    pub id: String,
    /// XADD 的 key=value 列表
    pub fields: Vec<(String, String)>,
}

impl RedisValue {
    pub fn is_nil(&self) -> bool {
        matches!(self, RedisValue::Nil)
    }

    /// 元素数量；标量返回 None
    pub fn len(&self) -> Option<usize> {
        match self {
            RedisValue::List(v) | RedisValue::Set(v) | RedisValue::Array(v) => Some(v.len()),
            RedisValue::Hash(v) => Some(v.len()),
            RedisValue::ZSet(v) => Some(v.len()),
            RedisValue::Stream(v) => Some(v.len()),
            _ => None,
        }
    }

    /// String/Bytes 当前已加载字节数；其它类型返回 None。
    pub fn scalar_byte_len(&self) -> Option<usize> {
        match self {
            RedisValue::Text(value) => Some(value.len()),
            RedisValue::Bytes(value) => Some(value.len()),
            _ => None,
        }
    }

    /// 标量固定返回 false
    pub fn is_empty(&self) -> bool {
        self.len().is_some_and(|n| n == 0)
    }

    /// UI 单行预览，截断长字符串
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
}

fn truncate(s: &str, max_len: usize) -> String {
    let mut chars = s.chars();
    let mut preview: String = chars.by_ref().take(max_len).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

/// 单行预览清洗：换行符（\n / \r）替换为空格。
/// GPUI 单行文本 shaping 断言不允许 \n（含 \n 直接 panic→abort）；仅用于显示预览。
/// 无换行时零拷贝
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
        // 含换行的 String 值预览必须压成单行，否则 key 详情渲染 panic
        let v = RedisValue::Text("line1\nline2\r\nline3".to_string());
        let p = v.display_preview(80);
        assert!(!p.contains('\n') && !p.contains('\r'));
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
