//! 工作区刷新队列与文件自写入标记。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::super::helpers::{FileTab, FileTabSource};
use super::super::vcs_view::VcsView;
use super::super::vcs_view_ops_repo::PF_FILE_SELF_WRITE_TTL;
use super::merge::path_matches_prefixes;
use crate::watcher::RepoRefresh;

pub(super) fn begin_workspace_refresh(
    in_flight: &mut bool,
    pending: &mut RepoRefresh,
    refresh: RepoRefresh,
) -> bool {
    if *in_flight {
        pending.merge(refresh);
        false
    } else {
        *in_flight = true;
        true
    }
}

pub(super) fn should_rerun(this: &VcsView, pending: &RepoRefresh) -> bool {
    this.repo.is_some() && !this.loading && !this.busy && !pending.is_empty()
}

/// watcher 命中本进程发起写入的同一代快照时，只刷新 Git 状态，不重载编辑器。
pub(super) fn take_recent_project_file_self_writes(
    markers: &mut HashMap<String, (u64, Instant)>,
    file_tabs: &[FileTab],
    event_prefixes: &HashSet<&str>,
    now: Instant,
) -> HashSet<String> {
    let mut consumed = HashSet::new();
    markers.retain(|path, (revision, saved_at)| {
        if now.saturating_duration_since(*saved_at) > PF_FILE_SELF_WRITE_TTL {
            return false;
        }
        if !path_matches_prefixes(path, event_prefixes) {
            return true;
        }
        let matches_revision = file_tabs.iter().any(|tab| {
            tab.path == *path
                && matches!(tab.source, FileTabSource::ProjectFiles)
                && tab
                    .cached_content
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.revision == *revision)
        });
        if matches_revision {
            consumed.insert(path.clone());
            false
        } else {
            true
        }
    });
    consumed
}

pub(super) fn merge_pending_refresh(pending: &std::sync::Mutex<RepoRefresh>, refresh: RepoRefresh) {
    match pending.lock() {
        Ok(mut pending) => pending.merge(refresh),
        Err(error) => {
            tracing::warn!(
                operation = "vcs_workspace_refresh",
                reason = "lock_poisoned"
            );
            error.into_inner().merge(refresh);
        }
    }
}

pub(super) fn take_pending_refresh(pending: &std::sync::Mutex<RepoRefresh>) -> RepoRefresh {
    match pending.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(error) => {
            tracing::warn!(
                operation = "vcs_workspace_refresh",
                reason = "lock_poisoned"
            );
            let mut pending = error.into_inner();
            std::mem::take(&mut *pending)
        }
    }
}

pub(super) fn enqueue_workspace_refresh(
    sender: &std::sync::Mutex<futures::channel::mpsc::Sender<()>>,
) {
    let mut sender = match sender.lock() {
        Ok(sender) => sender,
        Err(_) => {
            tracing::warn!(
                operation = "vcs_workspace_refresh",
                reason = "channel_lock_poisoned"
            );
            return;
        }
    };
    match sender.try_send(()) {
        Ok(()) => {}
        Err(error) if error.is_full() || error.is_disconnected() => {}
        Err(error) => {
            tracing::warn!(
                operation = "vcs_workspace_refresh",
                error = %error,
                "workspace refresh enqueue failed"
            );
        }
    }
}
