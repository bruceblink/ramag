//! 服务端分页游标。游标绑定账号、挂载点、前缀和请求代次，不能跨上下文复用。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use opendal::{Entry, EntryMode, Lister, Operator};
use parking_lot::Mutex;
use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_PAGE_ENTRIES, MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES, ObjectEntry,
    ObjectEntryKind, ObjectListCursor, ObjectListQuery, ObjectPage, ObjectStorageAccountSnapshot,
    ObjectStorageMount, is_opendal_safe_key, is_opendal_safe_list_prefix, is_opendal_safe_prefix,
};
use ramag_domain::error::ObjectStorageResult;

use crate::errors::{invalid, map_opendal};

const CURSOR_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_CURSORS: usize = 128;

struct CursorState {
    account_id: String,
    revision: u64,
    mount_id: String,
    prefix: String,
    generation: u64,
    lister: Lister,
    pending: Option<ObjectEntry>,
    emitted: usize,
    last_used: Instant,
}

#[derive(Default)]
pub struct CursorStore {
    states: Mutex<HashMap<String, CursorState>>,
}

impl CursorStore {
    pub async fn list_page(
        &self,
        operator: Arc<Operator>,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        query: &ObjectListQuery,
        cursor: Option<&ObjectListCursor>,
        generation: u64,
    ) -> ObjectStorageResult<ObjectPage> {
        if !is_opendal_safe_list_prefix(query.list_prefix()) {
            return Err(invalid("list", "当前前缀无法由 OpenDAL 安全表示"));
        }
        let mut state = match cursor {
            Some(cursor) => {
                self.take_bound(cursor, account, mount, query.list_prefix(), generation)?
            }
            None => {
                self.invalidate_mount(&account.id.to_string(), &mount.id.to_string());
                CursorState {
                    account_id: account.id.to_string(),
                    revision: account.revision,
                    mount_id: mount.id.to_string(),
                    prefix: query.list_prefix().to_string(),
                    generation,
                    lister: operator
                        .lister_with(query.list_prefix())
                        .limit(MAX_OBJECT_STORAGE_PAGE_ENTRIES)
                        .recursive(false)
                        .await
                        .map_err(|error| map_opendal("list", error))?,
                    pending: None,
                    emitted: 0,
                    last_used: Instant::now(),
                }
            }
        };

        let mut entries = Vec::with_capacity(MAX_OBJECT_STORAGE_PAGE_ENTRIES);
        if let Some(entry) = state.pending.take() {
            entries.push(entry);
        }
        while entries.len() <= MAX_OBJECT_STORAGE_PAGE_ENTRIES
            && state.emitted + entries.len() < MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES + 1
        {
            match state.lister.next().await {
                Some(Ok(entry)) => {
                    if let Some(entry) = to_entry(entry, query.directory_prefix()) {
                        entries.push(entry);
                    }
                }
                Some(Err(error)) => return Err(map_opendal("list", error)),
                None => break,
            }
        }

        let capped = state.emitted + entries.len() > MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES;
        if capped {
            entries.truncate(MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES - state.emitted);
        }
        let has_more = entries.len() > MAX_OBJECT_STORAGE_PAGE_ENTRIES;
        if has_more {
            let Some(extra) = entries.pop() else {
                return Err(invalid("list", "对象分页 lookahead 状态无效"));
            };
            state.pending = Some(extra);
        }
        state.emitted += entries.len();
        state.last_used = Instant::now();

        let next_cursor = if has_more && !capped {
            let cursor = ObjectListCursor::new();
            self.insert(cursor.clone(), state);
            Some(cursor)
        } else {
            None
        };
        Ok(ObjectPage {
            entries,
            next_cursor,
            capped,
        })
    }

    pub fn invalidate_account(&self, account_id: &str) {
        self.states
            .lock()
            .retain(|_, state| state.account_id != account_id);
    }

    pub fn clear(&self) {
        self.states.lock().clear();
    }

    fn invalidate_mount(&self, account_id: &str, mount_id: &str) {
        self.states
            .lock()
            .retain(|_, state| state.account_id != account_id || state.mount_id != mount_id);
    }

    fn take_bound(
        &self,
        cursor: &ObjectListCursor,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        prefix: &str,
        generation: u64,
    ) -> ObjectStorageResult<CursorState> {
        let now = Instant::now();
        let mut states = self.states.lock();
        states.retain(|_, state| now.duration_since(state.last_used) <= CURSOR_TTL);
        let state = states
            .remove(cursor.as_str())
            .ok_or_else(|| invalid("list", "分页游标已过期，请重新加载"))?;
        let bound = state.account_id == account.id.to_string()
            && state.revision == account.revision
            && state.mount_id == mount.id.to_string()
            && state.prefix == prefix
            && state.generation == generation;
        if !bound {
            return Err(invalid("list", "分页上下文已变化，请重新加载"));
        }
        Ok(state)
    }

    fn insert(&self, cursor: ObjectListCursor, state: CursorState) {
        let mut states = self.states.lock();
        if states.len() >= MAX_CURSORS
            && let Some(oldest) = states
                .iter()
                .min_by_key(|(_, state)| state.last_used)
                .map(|(key, _)| key.clone())
        {
            states.remove(&oldest);
        }
        states.insert(cursor.as_str().to_string(), state);
    }
}

fn to_entry(entry: Entry, prefix: &str) -> Option<ObjectEntry> {
    let key = entry.path().to_string();
    let metadata = entry.metadata();
    let kind = if metadata.mode() == EntryMode::DIR {
        ObjectEntryKind::Prefix
    } else {
        ObjectEntryKind::Object
    };
    let name = relative_display_name(&key, prefix)?;
    let operable = match kind {
        ObjectEntryKind::Prefix => is_opendal_safe_prefix(&key),
        ObjectEntryKind::Object => is_opendal_safe_key(&key),
    };
    Some(ObjectEntry {
        key,
        display_name: name,
        kind,
        operable,
        size: Some(metadata.content_length()),
        last_modified: metadata.last_modified().and_then(|value| {
            DateTime::parse_from_rfc3339(&value.to_string())
                .ok()
                .map(|value| value.with_timezone(&Utc))
        }),
        etag: metadata.etag().map(str::to_string),
        content_type: metadata.content_type().map(str::to_string),
        storage_class: None,
    })
}

fn relative_display_name(key: &str, prefix: &str) -> Option<String> {
    let name = key
        .strip_prefix(prefix)
        .unwrap_or(key)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(key);
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::relative_display_name;

    #[test]
    fn current_directory_marker_is_not_rendered_as_an_empty_child() {
        assert_eq!(relative_display_name("static/", "static/"), None);
        assert_eq!(
            relative_display_name("static/hydro_post/", "static/").as_deref(),
            Some("hydro_post")
        );
        assert_eq!(
            relative_display_name("static/file.txt", "static/").as_deref(),
            Some("file.txt")
        );
    }
}
