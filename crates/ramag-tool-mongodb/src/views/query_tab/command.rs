//! MongoDB 原生命令解析：目标集合、高危分类与 runCommand 应答归一化。

use ramag_domain::entities::MongoQueryResult;
use serde_json::Value;

/// 从 runCommand JSON 提取目标 collection（find/aggregate/insert 等命令名的字符串值）。
pub(super) fn extract_collection(command: &Value) -> Option<String> {
    const COMMAND_KEYS: &[&str] = &[
        "find",
        "aggregate",
        "count",
        "distinct",
        "insert",
        "update",
        "delete",
        "findAndModify",
    ];
    COMMAND_KEYS.iter().find_map(|key| {
        command
            .get(*key)
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

/// 仅拦截高影响、难恢复的原生命令；普通单文档写入仍直接执行。
pub(super) fn dangerous_command_reason(command: &Value) -> Option<String> {
    let object = command.as_object()?;
    if object.contains_key("dropDatabase") {
        return Some("将删除当前数据库及其中全部集合、索引和数据，不可撤销".into());
    }
    if object.contains_key("drop") {
        return Some("将删除整个集合及其全部文档和索引，不可撤销".into());
    }
    if object.contains_key("dropIndexes") {
        return Some("将删除集合索引，可能造成查询性能骤降或唯一约束失效".into());
    }
    if object.contains_key("renameCollection") {
        return Some("将重命名集合命名空间，可能使现有应用与查询失效".into());
    }
    if object.contains_key("shutdown") {
        return Some("将关闭 MongoDB 服务进程并断开所有客户端".into());
    }
    if let Some(deletes) = object.get("deletes").and_then(Value::as_array) {
        let broad = deletes.iter().any(|item| {
            let query_empty = item
                .get("q")
                .and_then(Value::as_object)
                .is_some_and(serde_json::Map::is_empty);
            let multi = item.get("limit").and_then(Value::as_i64) == Some(0);
            query_empty || multi
        });
        if broad {
            return Some("包含空条件或不限数量的批量删除，可能删除大量文档".into());
        }
    }
    if let Some(updates) = object.get("updates").and_then(Value::as_array) {
        let broad = updates.iter().any(|item| {
            let query_empty = item
                .get("q")
                .and_then(Value::as_object)
                .is_some_and(serde_json::Map::is_empty);
            let multi = item.get("multi").and_then(Value::as_bool) == Some(true);
            query_empty || multi
        });
        if broad {
            return Some("包含空条件或 multi=true 的批量更新，可能改动大量文档".into());
        }
    }
    if let Some(pipeline) = object.get("pipeline").and_then(Value::as_array)
        && pipeline
            .iter()
            .any(|stage| stage.get("$out").is_some() || stage.get("$merge").is_some())
    {
        return Some("聚合管道包含 $out/$merge，将写入或覆盖目标集合".into());
    }
    if object.get("remove").and_then(Value::as_bool) == Some(true)
        && object
            .get("query")
            .and_then(Value::as_object)
            .is_none_or(|query| query.is_empty())
    {
        return Some("findAndModify 使用空条件删除文档，目标可能不符合预期".into());
    }
    None
}

pub(super) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

pub(super) fn default_command_template() -> String {
    "{\n  \"ping\": 1\n}".to_string()
}

#[derive(Clone, Copy)]
pub(super) enum CommandResponseKind {
    Count,
    Insert,
    Update,
    Delete,
    Other,
}

pub(super) fn command_response_kind(command: &Value) -> CommandResponseKind {
    if command.get("count").is_some() {
        CommandResponseKind::Count
    } else if command.get("insert").is_some() {
        CommandResponseKind::Insert
    } else if command.get("update").is_some() {
        CommandResponseKind::Update
    } else if command.get("delete").is_some() {
        CommandResponseKind::Delete
    } else {
        CommandResponseKind::Other
    }
}

/// 根据原命令区分同名 `n` 字段，避免把写入结果误报为 count。
pub(super) fn parse_run_command_response(
    mut response: Value,
    elapsed_ms: u64,
    kind: CommandResponseKind,
) -> MongoQueryResult {
    let truncated = response
        .get("__ramag_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let memory_warning = response
        .get("__ramag_memory_warning")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let retained_bytes = response
        .get("__ramag_retained_bytes")
        .and_then(Value::as_u64)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .unwrap_or(0);
    if let Some(Value::Array(batch)) = response
        .get_mut("cursor")
        .and_then(Value::as_object_mut)
        .and_then(|cursor| cursor.get_mut("firstBatch"))
        .filter(|batch| batch.is_array())
        .map(std::mem::take)
    {
        let mut result =
            MongoQueryResult::read_with_budget(batch, elapsed_ms, truncated, retained_bytes);
        result.memory_warning |= memory_warning;
        return result;
    }
    match kind {
        CommandResponseKind::Count => {
            if let Some(count) = response.get("n").and_then(Value::as_u64) {
                return MongoQueryResult {
                    documents: vec![response],
                    affected: count,
                    elapsed_ms,
                    summary: format!("count={count}, {elapsed_ms}ms"),
                    truncated: false,
                    retained_bytes: 0,
                    memory_warning: false,
                };
            }
        }
        CommandResponseKind::Update => {
            if let Some(modified) = response
                .get("nModified")
                .and_then(Value::as_u64)
                .or_else(|| response.get("n").and_then(Value::as_u64))
            {
                return MongoQueryResult::write(modified, elapsed_ms, "update");
            }
        }
        CommandResponseKind::Insert | CommandResponseKind::Delete => {
            if let Some(affected) = response.get("n").and_then(Value::as_u64) {
                let operation = if matches!(kind, CommandResponseKind::Insert) {
                    "insert"
                } else {
                    "delete"
                };
                return MongoQueryResult::write(affected, elapsed_ms, operation);
            }
        }
        CommandResponseKind::Other => {}
    }
    MongoQueryResult::read(vec![response], elapsed_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_cursor_firstbatch() {
        let response = json!({
            "cursor": {"firstBatch": [{"a": 1}, {"a": 2}], "id": 0, "ns": "db.coll"},
            "ok": 1.0
        });
        let result = parse_run_command_response(response, 10, CommandResponseKind::Other);
        assert_eq!(result.documents.len(), 2);
    }

    #[test]
    fn parse_count_returns_n() {
        let result =
            parse_run_command_response(json!({"n": 42, "ok": 1.0}), 10, CommandResponseKind::Count);
        assert_eq!(result.affected, 42);
        assert!(result.summary.contains("count=42"));
    }

    #[test]
    fn parse_unknown_falls_back_to_single_doc() {
        let result = parse_run_command_response(
            json!({"ok": 1.0, "value": "x"}),
            5,
            CommandResponseKind::Other,
        );
        assert_eq!(result.documents.len(), 1);
    }

    #[test]
    fn update_uses_modified_count_instead_of_count_summary() {
        let result = parse_run_command_response(
            json!({"n": 5, "nModified": 3, "ok": 1.0}),
            8,
            CommandResponseKind::Update,
        );
        assert_eq!(result.affected, 3);
        assert!(result.summary.contains("update"));
    }

    #[test]
    fn update_falls_back_to_matched_count_when_modified_count_is_unusable() {
        let result = parse_run_command_response(
            json!({"n": 5, "nModified": null, "ok": 1.0}),
            8,
            CommandResponseKind::Update,
        );
        assert_eq!(result.affected, 5);
        assert!(result.summary.contains("update"));
    }

    #[test]
    fn high_risk_commands_require_confirmation() {
        for command in [
            json!({"dropDatabase": 1}),
            json!({"drop": "users"}),
            json!({"delete": "users", "deletes": [{"q": {}, "limit": 0}]}),
            json!({"update": "users", "updates": [{"q": {}, "u": {"$set": {"x": 1}}, "multi": true}]}),
            json!({"aggregate": "users", "pipeline": [{"$out": "backup"}], "cursor": {}}),
            json!({"findAndModify": "users", "remove": true}),
        ] {
            assert!(dangerous_command_reason(&command).is_some(), "{command}");
        }
    }

    #[test]
    fn scoped_reads_and_single_writes_do_not_prompt() {
        for command in [
            json!({"find": "users", "filter": {"active": true}}),
            json!({"delete": "users", "deletes": [{"q": {"_id": 1}, "limit": 1}]}),
            json!({"update": "users", "updates": [{"q": {"_id": 1}, "u": {"$set": {"x": 1}}, "multi": false}]}),
        ] {
            assert!(dangerous_command_reason(&command).is_none(), "{command}");
        }
    }
}
