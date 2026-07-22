//! MongoDB `find` 的客户端分页：编辑器保留原命令，每页按 skip/limit 派生执行命令。

use ramag_domain::entities::MongoQueryResult;
use serde_json::{Value, json};

pub(super) const MONGO_PAGE_SIZE: usize = 10_000;

#[derive(Clone)]
pub(super) struct MongoPager {
    base_command: Value,
    base_skip: u64,
    user_limit: Option<u64>,
    /// 每页相对原始 skip 的真实起点。结果因 256 MiB 提前截断时，下一页从实际
    /// 已展示文档数继续，不能机械跳过 10,000 条。
    page_offsets: Vec<u64>,
    pub(super) page: usize,
    pub(super) has_more: bool,
}

#[derive(Clone, Copy)]
pub(super) struct PageRequest {
    pub(super) page: usize,
    pub(super) page_size: usize,
    relative_offset: u64,
    visible_limit: usize,
    uses_sentinel: bool,
}

impl MongoPager {
    /// 仅对普通 `find` 自动分页；tailable / singleBatch / 负 limit 保持原生命令语义。
    pub(super) fn from_command(command: &Value) -> Option<Self> {
        let object = command.as_object()?;
        object.get("find")?.as_str()?;
        if ["tailable", "awaitData", "singleBatch"]
            .iter()
            .any(|key| object.get(*key).and_then(Value::as_bool) == Some(true))
        {
            return None;
        }
        let base_skip = match object.get("skip") {
            None => 0,
            Some(value) => u64::try_from(value.as_i64()?).ok()?,
        };
        let user_limit = match object.get("limit") {
            None => None,
            Some(value) if value.as_i64() == Some(0) => None,
            Some(value) => Some(u64::try_from(value.as_i64()?).ok()?),
        };
        Some(Self {
            base_command: command.clone(),
            base_skip,
            user_limit,
            page_offsets: vec![0],
            page: 0,
            has_more: false,
        })
    }

    pub(super) fn command_for_page(&self, page: usize) -> Result<(Value, PageRequest), String> {
        let relative_offset = self
            .page_offsets
            .get(page)
            .copied()
            .ok_or_else(|| "MongoDB 分页只能逐页前进或返回已加载页".to_string())?;
        let remaining = self
            .user_limit
            .map(|limit| limit.saturating_sub(relative_offset));
        if remaining == Some(0) {
            return Err("已达到原命令 limit 指定的末尾".into());
        }
        let visible_limit = remaining
            .map(|remaining| remaining.min(MONGO_PAGE_SIZE as u64) as usize)
            .unwrap_or(MONGO_PAGE_SIZE);
        let uses_sentinel = remaining.is_none_or(|remaining| remaining > visible_limit as u64);
        let fetch_limit = visible_limit
            .checked_add(usize::from(uses_sentinel))
            .ok_or_else(|| "MongoDB 分页读取数量溢出".to_string())?;
        let skip = self
            .base_skip
            .checked_add(relative_offset)
            .ok_or_else(|| "MongoDB skip 超出 u64 范围".to_string())?;
        let skip = i64::try_from(skip).map_err(|_| "MongoDB skip 超出 i64 范围".to_string())?;

        let mut command = self.base_command.clone();
        let object = command
            .as_object_mut()
            .ok_or_else(|| "MongoDB find 命令必须是 JSON 对象".to_string())?;
        object.insert("skip".into(), json!(skip));
        object.insert("limit".into(), json!(fetch_limit));
        object.insert("batchSize".into(), json!(fetch_limit));
        Ok((
            command,
            PageRequest {
                page,
                page_size: MONGO_PAGE_SIZE,
                relative_offset,
                visible_limit,
                uses_sentinel,
            },
        ))
    }

    pub(super) fn base_command(&self) -> &Value {
        &self.base_command
    }

    /// 记录本页实际消费数量，生成下一页的精确 offset。
    pub(super) fn finish_request(
        &mut self,
        request: PageRequest,
        displayed: usize,
        mut has_more: bool,
    ) {
        let displayed = u64::try_from(displayed).unwrap_or(u64::MAX);
        let next_offset = request.relative_offset.saturating_add(displayed);
        // 避免异常空页造成“下一页”原地循环。
        if next_offset == request.relative_offset {
            has_more = false;
        }
        self.page = request.page;
        self.has_more = has_more;
        self.page_offsets.truncate(request.page.saturating_add(1));
        if has_more {
            self.page_offsets.push(next_offset);
        }
    }

    pub(super) fn accepts_adjacent_page(&self, requested_page: usize) -> bool {
        requested_page
            .checked_add(1)
            .is_some_and(|page| page == self.page)
            || (self
                .page
                .checked_add(1)
                .is_some_and(|page| page == requested_page)
                && self.has_more)
    }
}

pub(super) fn finish_page(result: &mut MongoQueryResult, request: PageRequest) -> bool {
    let has_more = result.truncated
        || (request.uses_sentinel && result.documents.len() > request.visible_limit);
    if result.documents.len() > request.visible_limit {
        result.documents.truncate(request.visible_limit);
        result.retained_bytes = ramag_domain::entities::mongo_documents_retained_bytes(
            &result.documents,
            result.documents.capacity(),
        );
        result.memory_warning =
            result.retained_bytes >= ramag_domain::entities::INTERACTIVE_RESULT_WARNING_BYTES;
    }
    let displayed = result.documents.len();
    result.summary = if result.truncated {
        format!(
            "已加载前 {displayed} 条（结果被截断）, {}ms",
            result.elapsed_ms
        )
    } else {
        format!("{displayed} docs, {}ms", result.elapsed_ms)
    };
    has_more
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_find_uses_ten_thousand_plus_sentinel() {
        let pager = MongoPager::from_command(&json!({"find": "users", "filter": {}})).unwrap();

        let (command, request) = pager.command_for_page(0).unwrap();

        assert_eq!(command["skip"], 0);
        assert_eq!(command["limit"], 10_001);
        assert_eq!(request.visible_limit, 10_000);
        assert!(request.uses_sentinel);
    }

    #[test]
    fn original_skip_and_limit_are_preserved_across_pages() {
        let mut pager = MongoPager::from_command(&json!({
            "find": "users",
            "skip": 7,
            "limit": 15_000
        }))
        .unwrap();

        let (first, first_request) = pager.command_for_page(0).unwrap();
        pager.finish_request(first_request, 10_000, true);
        let (second, second_request) = pager.command_for_page(1).unwrap();

        assert_eq!(first["skip"], 7);
        assert_eq!(first["limit"], 10_001);
        assert!(first_request.uses_sentinel);
        assert_eq!(second["skip"], 10_007);
        assert_eq!(second["limit"], 5_000);
        assert!(!second_request.uses_sentinel);
        assert!(pager.command_for_page(2).is_err());
    }

    #[test]
    fn memory_truncated_page_continues_after_actual_documents() {
        let mut pager = MongoPager::from_command(&json!({"find": "users", "skip": 7})).unwrap();
        let (_, request) = pager.command_for_page(0).unwrap();
        let mut result = MongoQueryResult::read_maybe_truncated(
            (0..321).map(|id| json!({"_id": id})).collect(),
            1,
            true,
        );
        let has_more = finish_page(&mut result, request);
        pager.finish_request(request, result.documents.len(), has_more);

        let (second, _) = pager.command_for_page(1).unwrap();

        assert_eq!(second["skip"], 328);
    }

    #[test]
    fn special_find_modes_are_not_rewritten() {
        assert!(MongoPager::from_command(&json!({"find": "events", "tailable": true})).is_none());
        assert!(MongoPager::from_command(&json!({"find": "users", "limit": -10})).is_none());
        assert!(MongoPager::from_command(&json!({"find": "users", "skip": u64::MAX})).is_none());
    }

    #[test]
    fn sentinel_is_removed_before_display() {
        let pager = MongoPager::from_command(&json!({"find": "users"})).unwrap();
        let (_, request) = pager.command_for_page(0).unwrap();
        let mut result =
            MongoQueryResult::read((0..10_001).map(|id| json!({"_id": id})).collect(), 1);

        assert!(finish_page(&mut result, request));
        assert_eq!(result.documents.len(), 10_000);
        assert_eq!(result.summary, "10000 docs, 1ms");
    }
}
