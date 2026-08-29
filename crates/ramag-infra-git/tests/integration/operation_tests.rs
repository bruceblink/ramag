use super::*;

#[test]
fn branch_checkout_merge() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    let main = current_branch(&driver, &id);

    block_on(driver.create_branch(&id, "feature", None)).unwrap();
    block_on(driver.checkout(&id, "feature")).unwrap();
    commit_file(&driver, &id, tmp.path(), "b.txt", "feat\n", "feat commit");

    block_on(driver.checkout(&id, &main)).unwrap();
    block_on(driver.merge(
        &id,
        "feature",
        true,
        false,
        Some("merge feature\n\nfrom stdin"),
    ))
    .unwrap();

    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log[0].subject, "merge feature");
    assert!(
        log.iter().any(|c| c.subject == "feat commit"),
        "merge 后历史应含 feature commit"
    );
}

#[test]
fn branch_created_from_remote_explicitly_tracks_upstream() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    let main = current_branch(&driver, &id);
    let bare = tempfile::TempDir::new().unwrap();
    let initialized = std::process::Command::new("git")
        .args(["init", "--bare", bare.path().to_str().unwrap()])
        .status()
        .unwrap()
        .success();
    assert!(initialized, "初始化 bare remote 失败");
    block_on(driver.add_remote(&id, "origin", bare.path().to_str().unwrap())).unwrap();
    block_on(driver.push(&id, "origin", &main, true, false)).unwrap();

    block_on(driver.create_branch(&id, "feature", None)).unwrap();
    block_on(driver.checkout(&id, "feature")).unwrap();
    commit_file(
        &driver,
        &id,
        tmp.path(),
        "feature.txt",
        "feature\n",
        "feature",
    );
    block_on(driver.push(&id, "origin", "feature", true, false)).unwrap();
    block_on(driver.checkout(&id, &main)).unwrap();
    block_on(driver.delete_branch(&id, "feature", true)).unwrap();
    block_on(driver.fetch(&id, "origin")).unwrap();

    block_on(driver.create_branch(&id, "feature", Some("origin/feature"))).unwrap();
    let branches = block_on(driver.list_branches(&id, BranchKind::Local)).unwrap();
    let feature = branches
        .iter()
        .find(|branch| branch.name == "feature")
        .expect("应创建本地 feature");
    assert_eq!(feature.upstream.as_deref(), Some("origin/feature"));

    let (local, remote) = block_on(driver.list_all_branches(&id)).unwrap();
    assert!(local.iter().any(|branch| branch.name == "feature"));
    assert!(remote.iter().any(|branch| branch.name == "origin/feature"));
}

#[test]
fn stash_save_apply() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");

    write(tmp.path(), "a.txt", "base\nmodified\n");
    block_on(driver.stash_save(&id, Some("wip"), false)).unwrap();
    let st = block_on(driver.status(&id)).unwrap();
    assert!(st.files.is_empty(), "stash 后工作区应干净");

    let stashes = block_on(driver.list_stashes(&id)).unwrap();
    assert_eq!(stashes.len(), 1, "应有 1 条 stash");

    block_on(driver.stash_apply(&id, 0, false)).unwrap();
    let st = block_on(driver.status(&id)).unwrap();
    assert!(!st.files.is_empty(), "apply 后改动应回来");
}

#[test]
fn tag_create_list() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");

    block_on(driver.create_tag(&id, "v1.0", None, Some("release"), false)).unwrap();
    let tags = block_on(driver.list_tags(&id)).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "v1.0");
}

/// 验证显式 tag 目标会指向指定的历史 commit，而不是当前 HEAD。
#[test]
fn tag_create_at_explicit_target() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "first\n", "first");
    let first = block_on(driver.log(&id, LogOptions::default()))
        .unwrap()
        .first()
        .expect("首个 commit 应存在")
        .id
        .0
        .clone();
    commit_file(&driver, &id, tmp.path(), "a.txt", "second\n", "second");

    block_on(driver.create_tag(&id, "v-first", Some(&first), None, false)).unwrap();
    let tags = block_on(driver.list_tags(&id)).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].commit.0, first);
}

#[test]
fn reset_and_revert() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "v1\n", "c1");
    commit_file(&driver, &id, tmp.path(), "a.txt", "v2\n", "c2");

    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log.len(), 2);

    // revert 最新 commit（c2）
    let c2 = log[0].id.0.clone();
    block_on(driver.revert(&id, &c2)).unwrap();
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log.len(), 3, "revert 应新增一个 commit");

    // reset --hard 回 c1
    let c1 = log.last().unwrap().id.0.clone();
    block_on(driver.reset(&id, &c1, ResetKind::Hard)).unwrap();
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log.len(), 1, "reset 后只剩 c1");
}

#[test]
fn revert_conflict_then_abort() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "v1\n", "c1");
    commit_file(&driver, &id, tmp.path(), "a.txt", "v2\n", "c2");
    // c3 再改同一行：revert c2 时与 c3 的内容冲突
    commit_file(&driver, &id, tmp.path(), "a.txt", "v3\n", "c3");

    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    let c2 = log[1].id.0.clone();
    assert!(
        block_on(driver.revert(&id, &c2)).is_err(),
        "revert 应因冲突失败"
    );
    let status = block_on(driver.status(&id)).unwrap();
    assert_eq!(
        status.operation,
        Some(ramag_domain::entities::RepoOperation::Revert),
        "冲突后应处于 revert 进行中"
    );

    block_on(driver.revert_abort(&id)).unwrap();
    let status = block_on(driver.status(&id)).unwrap();
    assert_eq!(status.operation, None, "abort 后应回到干净状态");
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log.len(), 3, "abort 不应产生新 commit");
}

#[test]
fn revert_conflict_can_continue_without_editor() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "v1\n", "c1");
    commit_file(&driver, &id, tmp.path(), "a.txt", "v2\n", "c2");
    commit_file(&driver, &id, tmp.path(), "a.txt", "v3\n", "c3");
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    let c2 = log[1].id.0.clone();

    assert!(block_on(driver.revert(&id, &c2)).is_err());
    block_on(driver.use_theirs(&id, &["a.txt".to_string()])).unwrap();
    block_on(driver.revert_continue(&id))
        .expect("解决冲突后 revert --continue 应成功且不打开编辑器");
    assert_eq!(block_on(driver.status(&id)).unwrap().operation, None);
    assert_eq!(
        block_on(driver.log(&id, LogOptions::default()))
            .unwrap()
            .len(),
        4,
        "continue 应生成 revert commit"
    );
}

#[test]
fn cherry_pick_commit() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    let main = current_branch(&driver, &id);

    block_on(driver.create_branch(&id, "feature", None)).unwrap();
    block_on(driver.checkout(&id, "feature")).unwrap();
    commit_file(&driver, &id, tmp.path(), "b.txt", "feat\n", "feat-commit");
    let feat = block_on(driver.log(&id, LogOptions::default())).unwrap()[0]
        .id
        .0
        .clone();

    block_on(driver.checkout(&id, &main)).unwrap();
    block_on(driver.cherry_pick(&id, &feat)).unwrap();
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert!(log.iter().any(|c| c.subject == "feat-commit"));
}

#[test]
fn diff_and_blame() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "l1\nl2\n", "init");
    write(tmp.path(), "a.txt", "l1\nl2\nl3\n");

    let diff = block_on(driver.diff_file(&id, "a.txt", DiffKind::WorkingTreeVsIndex)).unwrap();
    assert!(!diff.hunks.is_empty(), "diff 应有 hunk");

    block_on(driver.stage(&id, &["a.txt".to_string()])).unwrap();
    block_on(driver.commit(&id, "c2", false, false)).unwrap();
    let blame = block_on(driver.blame(&id, "a.txt")).unwrap();
    assert_eq!(blame.len(), 3, "blame 行数应等于文件行数");
}

#[test]
fn merge_conflict_can_continue_without_editor() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    let main = current_branch(&driver, &id);

    block_on(driver.create_branch(&id, "feature", None)).unwrap();
    block_on(driver.checkout(&id, "feature")).unwrap();
    commit_file(
        &driver,
        &id,
        tmp.path(),
        "a.txt",
        "feature-change\n",
        "feat",
    );

    block_on(driver.checkout(&id, &main)).unwrap();
    commit_file(&driver, &id, tmp.path(), "a.txt", "main-change\n", "main");

    // 冲突 merge：应返回 Err 或进入冲突状态（status.operation = Merge）
    let _ = block_on(driver.merge(&id, "feature", false, false, None));
    let st = block_on(driver.status(&id)).unwrap();
    let has_conflict = st.files.iter().any(|f| {
        matches!(
            f.staged,
            Some(ramag_domain::entities::FileChangeKind::Conflicted)
        )
    });
    assert!(
        has_conflict || st.operation.is_some(),
        "冲突 merge 后应检测到冲突文件或进行中操作"
    );
    let content = block_on(driver.get_conflict_content(&id, "a.txt")).unwrap();
    assert!(content.base.join("\n").contains("base"));
    assert!(content.ours.join("\n").contains("main-change"));
    assert!(content.theirs.join("\n").contains("feature-change"));
    block_on(driver.use_ours(&id, &["a.txt".to_string()])).unwrap();
    block_on(driver.merge_continue(&id)).expect("解决冲突后 merge --continue 应成功且不打开编辑器");
    let status = block_on(driver.status(&id)).unwrap();
    assert_eq!(status.operation, None);
}

#[test]
fn cherry_pick_conflict_can_continue_without_editor() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    let main = current_branch(&driver, &id);
    block_on(driver.create_branch(&id, "feature", None)).unwrap();
    block_on(driver.checkout(&id, "feature")).unwrap();
    commit_file(&driver, &id, tmp.path(), "a.txt", "feature\n", "feature");
    let feature_commit = block_on(driver.log(&id, LogOptions::default())).unwrap()[0]
        .id
        .0
        .clone();
    block_on(driver.checkout(&id, &main)).unwrap();
    commit_file(&driver, &id, tmp.path(), "a.txt", "main\n", "main");

    assert!(block_on(driver.cherry_pick(&id, &feature_commit)).is_err());
    block_on(driver.use_theirs(&id, &["a.txt".to_string()])).unwrap();
    block_on(driver.cherry_pick_continue(&id))
        .expect("解决冲突后 cherry-pick --continue 应成功且不打开编辑器");
    assert_eq!(block_on(driver.status(&id)).unwrap().operation, None);
}

#[test]
fn rebase_conflict_can_continue_without_editor() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    let main = current_branch(&driver, &id);
    block_on(driver.create_branch(&id, "feature", None)).unwrap();
    block_on(driver.checkout(&id, "feature")).unwrap();
    commit_file(&driver, &id, tmp.path(), "a.txt", "feature\n", "feature");
    block_on(driver.checkout(&id, &main)).unwrap();
    commit_file(&driver, &id, tmp.path(), "a.txt", "main\n", "main");
    block_on(driver.checkout(&id, "feature")).unwrap();

    assert!(block_on(driver.rebase(&id, &main)).is_err());
    block_on(driver.use_theirs(&id, &["a.txt".to_string()])).unwrap();
    block_on(driver.rebase_continue(&id))
        .expect("解决冲突后 rebase --continue 应成功且不打开编辑器");
    assert_eq!(block_on(driver.status(&id)).unwrap().operation, None);
}

#[test]
fn rebase_onto_branch() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "c1");
    let main = current_branch(&driver, &id);

    block_on(driver.create_branch(&id, "feature", None)).unwrap();
    block_on(driver.checkout(&id, "feature")).unwrap();
    commit_file(&driver, &id, tmp.path(), "f.txt", "feat\n", "feat");

    block_on(driver.checkout(&id, &main)).unwrap();
    commit_file(&driver, &id, tmp.path(), "m.txt", "main\n", "main-commit");

    block_on(driver.checkout(&id, "feature")).unwrap();
    block_on(driver.rebase(&id, &main)).expect("rebase 应成功");
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert!(
        log.iter().any(|c| c.subject == "main-commit"),
        "rebase 后 feature 应含 main 的 commit"
    );
    assert!(log.iter().any(|c| c.subject == "feat"));
}

/// interactive rebase：drop 中间 commit，验证 execute 的 stderr 判定真机可用
#[test]
fn interactive_rebase_drop() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "1\n", "c1");
    commit_file(&driver, &id, tmp.path(), "b.txt", "2\n", "c2");
    commit_file(&driver, &id, tmp.path(), "c.txt", "3\n", "c3");

    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    let c1 = log.last().unwrap().id.0.clone();

    let mut plan = block_on(driver.interactive_rebase_plan(&id, &c1)).unwrap();
    assert_eq!(plan.len(), 2, "c1..HEAD 应有 c2,c3");
    // plan 最老在前：plan[0]=c2，标记 Drop
    plan[0].action = ramag_domain::entities::RebaseAction::Drop;
    block_on(driver.interactive_rebase_execute(&id, &c1, &plan))
        .expect("interactive rebase execute");

    let log2 = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log2.len(), 2, "drop c2 后应剩 c1,c3");
    assert!(!log2.iter().any(|c| c.subject == "c2"), "c2 应被 drop");
    assert!(log2.iter().any(|c| c.subject == "c3"), "c3 应保留");
}
