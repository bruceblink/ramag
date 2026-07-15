//! 查询编辑器草稿的轻量持久化结构。
//! 只保存用户手写文本与所属连接上下文，不保存查询结果或自动浏览语句。

use serde::{Deserialize, Serialize};

/// 单个数据库会话允许的查询编辑器上限；同时约束运行时实体数量与恢复数量。
pub const MAX_EDITOR_TABS: usize = 32;
/// 单条草稿最大 4 MiB；正常 SQL / Mongo 命令远小于此值。
const MAX_DRAFT_BYTES: usize = 4 * 1024 * 1024;
/// 一个连接全部草稿最大 16 MiB。
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorDraftPref {
    pub title: String,
    pub text: String,
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
        let mut pref: Self =
            serde_json::from_str(json).map_err(|e| format!("草稿数据格式无效：{e}"))?;
        if pref.drafts.len() > MAX_EDITOR_TABS {
            return Err(format!(
                "草稿标签过多：{} > {MAX_EDITOR_TABS}",
                pref.drafts.len()
            ));
        }
        let mut total = 0usize;
        for draft in &pref.drafts {
            let bytes = draft.text.len();
            if bytes > MAX_DRAFT_BYTES {
                return Err(format!("单条草稿过大：{bytes} bytes"));
            }
            total = total.saturating_add(bytes);
        }
        if total > MAX_TOTAL_BYTES {
            return Err(format!("草稿总量过大：{total} bytes"));
        }
        pref.active = pref.active.min(pref.drafts.len().saturating_sub(1));
        Ok(pref)
    }
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
    fn runtime_tab_limit_has_explicit_boundary() {
        assert!(can_open_editor_tab(MAX_EDITOR_TABS - 1));
        assert!(!can_open_editor_tab(MAX_EDITOR_TABS));
        assert!(!can_open_editor_tab(MAX_EDITOR_TABS + 1));
    }
}
