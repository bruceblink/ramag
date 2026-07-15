//! 查询编辑器草稿的轻量持久化结构。
//! 只保存用户手写文本与所属连接上下文，不保存查询结果或自动浏览语句。

use std::io;

use gpui::SharedString;
use serde::{Deserialize, Serialize};

/// 单个数据库会话允许的查询编辑器上限；同时约束运行时实体数量与恢复数量。
pub const MAX_EDITOR_TABS: usize = 32;
/// 单条草稿最大 4 MiB；正常 SQL / Mongo 命令远小于此值。
pub const MAX_EDITOR_DRAFT_BYTES: usize = 4 * 1024 * 1024;
/// 一个连接全部草稿最大 16 MiB。
pub const MAX_EDITOR_WORKSPACE_TEXT_BYTES: usize = 16 * 1024 * 1024;
/// 序列化后的偏好不得超过 storage 的单值上限；转义膨胀时提前停止写缓冲区。
pub const MAX_EDITOR_WORKSPACE_PREF_BYTES: usize = 16 * 1024 * 1024;
const MAX_EDITOR_DRAFT_TITLE_BYTES: usize = 256;
const MAX_EDITOR_DRAFT_CONTEXT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorDraftPref {
    pub title: String,
    /// `SharedString` 让连续输入的防抖快照只增加引用，不复制整段 SQL / JSON。
    pub text: SharedString,
    /// SQL 为默认 schema，MongoDB 为 database。
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorWorkspacePref {
    pub active: usize,
    pub drafts: Vec<EditorDraftPref>,
}

impl EditorWorkspacePref {
    pub fn new(active: usize, drafts: Vec<EditorDraftPref>) -> Self {
        let active = active.min(drafts.len().saturating_sub(1));
        Self { active, drafts }
    }

    /// 解析并校验本地偏好，损坏或异常膨胀时显式报错，不把坏数据送进 UI。
    pub fn parse(json: &str) -> Result<Self, String> {
        if json.len() > MAX_EDITOR_WORKSPACE_PREF_BYTES {
            return Err(format!("草稿恢复数据过大：{} bytes", json.len()));
        }
        let mut pref: Self =
            serde_json::from_str(json).map_err(|e| format!("草稿数据格式无效：{e}"))?;
        pref.validate()?;
        pref.active = pref.active.min(pref.drafts.len().saturating_sub(1));
        Ok(pref)
    }

    /// 校验内存快照；持久化前调用，避免先生成一个注定无法写入的大 JSON。
    pub fn validate(&self) -> Result<(), String> {
        if self.drafts.len() > MAX_EDITOR_TABS {
            return Err(format!(
                "草稿标签过多：{} > {MAX_EDITOR_TABS}",
                self.drafts.len()
            ));
        }
        let mut total = 0usize;
        for draft in &self.drafts {
            if draft.title.len() > MAX_EDITOR_DRAFT_TITLE_BYTES
                || draft.title.chars().any(char::is_control)
            {
                return Err("草稿标签标题过长或包含控制字符".into());
            }
            if draft.context.as_ref().is_some_and(|context| {
                context.len() > MAX_EDITOR_DRAFT_CONTEXT_BYTES || context.contains('\0')
            }) {
                return Err("草稿数据库上下文过长或包含 NUL 字符".into());
            }
            let bytes = draft.text.len();
            if bytes > MAX_EDITOR_DRAFT_BYTES {
                return Err(format!("单条草稿过大：{bytes} bytes"));
            }
            total = total.saturating_add(bytes);
        }
        if total > MAX_EDITOR_WORKSPACE_TEXT_BYTES {
            return Err(format!("草稿总量过大：{total} bytes"));
        }
        Ok(())
    }

    /// 有界序列化：JSON 转义可能放大文本，达到偏好上限即停止，不制造超大临时字符串。
    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serialize_json_with_limit(self, MAX_EDITOR_WORKSPACE_PREF_BYTES)
    }
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
    overflowed: bool,
}

impl LimitedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(64 * 1024)),
            limit,
            overflowed: false,
        }
    }
}

impl io::Write for LimitedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(buf.len()) > self.limit {
            self.overflowed = true;
            return Err(io::Error::other("editor workspace JSON exceeds limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_json_with_limit<T: Serialize>(value: &T, limit: usize) -> Result<String, String> {
    let mut writer = LimitedWriter::new(limit);
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.overflowed {
            return Err(format!("草稿持久化数据超过 {limit} bytes 上限"));
        }
        return Err(format!("草稿序列化失败：{error}"));
    }
    String::from_utf8(writer.bytes).map_err(|error| format!("草稿序列化结果不是 UTF-8：{error}"))
}

/// 运行时能否继续创建查询编辑器；与持久化恢复共用同一资源边界。
pub fn can_open_editor_tab(current: usize) -> bool {
    current < MAX_EDITOR_TABS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clamps_active_index() {
        let json =
            r#"{"active":9,"drafts":[{"title":"查询 1","text":"select 1","context":"main"}]}"#;
        assert_eq!(
            EditorWorkspacePref::parse(json).map(|pref| pref.active),
            Ok(0)
        );
    }

    #[test]
    fn parse_rejects_too_many_tabs() {
        let drafts = (0..=MAX_EDITOR_TABS)
            .map(|i| EditorDraftPref {
                title: format!("查询 {i}"),
                text: "select 1".into(),
                context: None,
            })
            .collect();
        let json = serde_json::to_string(&EditorWorkspacePref::new(0, drafts));
        assert!(json.is_ok_and(|json| EditorWorkspacePref::parse(&json).is_err()));
    }

    #[test]
    fn bounded_serialization_stops_on_escape_expansion() {
        let pref = EditorWorkspacePref::new(
            0,
            vec![EditorDraftPref {
                title: "查询 1".into(),
                text: "\u{0001}".repeat(32).into(),
                context: None,
            }],
        );
        assert!(serialize_json_with_limit(&pref, 64).is_err());
    }

    #[test]
    fn parse_rejects_oversized_raw_or_metadata_fields() {
        assert!(
            EditorWorkspacePref::parse(&" ".repeat(MAX_EDITOR_WORKSPACE_PREF_BYTES + 1)).is_err()
        );
        let pref = EditorWorkspacePref::new(
            0,
            vec![EditorDraftPref {
                title: "x".repeat(MAX_EDITOR_DRAFT_TITLE_BYTES + 1),
                text: "select 1".into(),
                context: None,
            }],
        );
        assert!(pref.validate().is_err());
    }

    #[test]
    fn runtime_tab_limit_has_explicit_boundary() {
        assert!(can_open_editor_tab(MAX_EDITOR_TABS - 1));
        assert!(!can_open_editor_tab(MAX_EDITOR_TABS));
        assert!(!can_open_editor_tab(MAX_EDITOR_TABS + 1));
    }
}
