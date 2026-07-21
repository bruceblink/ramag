//! History 提交缓存的条数与内存预算；分页追加只复制 `Rc`，不深拷贝旧提交正文。

use std::rc::Rc;

use ramag_domain::entities::Commit;

const MAX_HISTORY_COMMITS: usize = 100_000;
const MAX_HISTORY_RETAINED_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct RetainedHistory {
    pub(super) commits: Vec<Rc<Commit>>,
    pub(super) retained_bytes: usize,
    pub(super) limit_reached: bool,
}

pub(super) fn replace(commits: Vec<Commit>) -> RetainedHistory {
    retain_with_limits(
        &[],
        0,
        commits,
        MAX_HISTORY_COMMITS,
        MAX_HISTORY_RETAINED_BYTES,
    )
}

pub(super) fn append(
    existing: &[Rc<Commit>],
    retained_bytes: usize,
    commits: Vec<Commit>,
) -> RetainedHistory {
    retain_with_limits(
        existing,
        retained_bytes,
        commits,
        MAX_HISTORY_COMMITS,
        MAX_HISTORY_RETAINED_BYTES,
    )
}

fn retain_with_limits(
    existing: &[Rc<Commit>],
    retained_bytes: usize,
    incoming: Vec<Commit>,
    max_commits: usize,
    max_bytes: usize,
) -> RetainedHistory {
    let incoming_len = incoming.len();
    let capacity = existing.len().saturating_add(incoming_len).min(max_commits);
    let mut commits = Vec::with_capacity(capacity);
    commits.extend(existing.iter().cloned());
    let mut bytes = retained_bytes;
    let mut accepted = 0usize;

    for commit in incoming {
        if commits.len() >= max_commits {
            break;
        }
        let payload_bytes = commit_retained_bytes(&commit);
        let Some(next_bytes) = bytes.checked_add(payload_bytes) else {
            break;
        };
        // 单条异常大的首个 commit 仍保留，避免历史区变成空白；后续条目停止追加。
        if !commits.is_empty() && next_bytes > max_bytes {
            break;
        }
        commits.push(Rc::new(commit));
        bytes = next_bytes;
        accepted += 1;
    }

    let limit_reached = accepted < incoming_len
        || commits.len() >= max_commits
        || (!commits.is_empty() && bytes >= max_bytes);
    RetainedHistory {
        commits,
        retained_bytes: bytes,
        limit_reached,
    }
}

fn commit_retained_bytes(commit: &Commit) -> usize {
    let mut total = std::mem::size_of::<Commit>()
        .saturating_add(commit.id.0.capacity())
        .saturating_add(commit.author.name.capacity())
        .saturating_add(commit.author.email.capacity())
        .saturating_add(commit.committer.name.capacity())
        .saturating_add(commit.committer.email.capacity())
        .saturating_add(commit.subject.capacity())
        .saturating_add(commit.body.capacity())
        .saturating_add(
            commit
                .parents
                .capacity()
                .saturating_mul(std::mem::size_of::<ramag_domain::entities::CommitId>()),
        )
        .saturating_add(
            commit
                .refs
                .capacity()
                .saturating_mul(std::mem::size_of::<String>()),
        );
    for parent in &commit.parents {
        total = total.saturating_add(parent.0.capacity());
    }
    for reference in &commit.refs {
        total = total.saturating_add(reference.capacity());
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ramag_domain::entities::{CommitId, Signature};

    fn commit(id: &str, body_bytes: usize) -> Commit {
        let signature = Signature {
            name: "Author".into(),
            email: "author@example.com".into(),
            timestamp: Utc::now(),
        };
        Commit {
            id: CommitId(id.into()),
            parents: Vec::new(),
            author: signature.clone(),
            committer: signature,
            subject: format!("commit {id}"),
            body: "x".repeat(body_bytes),
            refs: Vec::new(),
        }
    }

    #[test]
    fn count_limit_keeps_prefix_and_reports_boundary() {
        let retained = retain_with_limits(
            &[],
            0,
            vec![commit("1", 0), commit("2", 0), commit("3", 0)],
            2,
            usize::MAX,
        );

        assert_eq!(retained.commits.len(), 2);
        assert!(retained.limit_reached);
    }

    #[test]
    fn append_reuses_existing_commit_allocation_and_bounds_bytes() {
        let existing = Rc::new(commit("1", 32));
        let existing_bytes = commit_retained_bytes(&existing);
        let retained = retain_with_limits(
            std::slice::from_ref(&existing),
            existing_bytes,
            vec![commit("2", 128)],
            10,
            existing_bytes,
        );

        assert_eq!(retained.commits.len(), 1);
        assert!(Rc::ptr_eq(&retained.commits[0], &existing));
        assert!(retained.limit_reached);
    }
}
