//! Git 集成测试分组。

use super::*;

#[test]
fn close_repo_releases_handle_and_allows_clean_reopen() {
    let (driver, id, tmp) = setup();

    block_on(driver.close_repo(&id)).unwrap();
    assert!(block_on(driver.status(&id)).is_err());

    let reopened = block_on(driver.open_repo(tmp.path())).unwrap();
    assert_ne!(reopened.id, id);
    assert!(block_on(driver.status(&reopened.id)).is_ok());
}

#[test]
fn concurrent_open_repo_reuses_one_handle() {
    let tmp = tempfile::TempDir::new().unwrap();
    let initializer = GitDriverImpl::new();
    block_on(initializer.init_repo(tmp.path())).unwrap();
    drop(initializer);

    let driver = GitDriverImpl::new();
    let opened = block_on(join_all((0..32).map(|_| driver.open_repo(tmp.path()))))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let first_id = &opened[0].id;
    assert!(opened.iter().all(|config| &config.id == first_id));
    assert!(block_on(driver.status(first_id)).is_ok());
}

#[test]
fn init_stage_commit_log() {
    let (driver, id, tmp) = setup();
    write(tmp.path(), "a.txt", "line1\nline2\n");

    let st = block_on(driver.status(&id)).unwrap();
    assert_eq!(st.files.len(), 1, "应有 1 个 untracked");

    block_on(driver.stage(&id, &["a.txt".to_string()])).unwrap();
    let st = block_on(driver.status(&id)).unwrap();
    assert!(st.files[0].staged.is_some(), "stage 后应 staged");

    let cid = block_on(driver.commit(&id, "first commit\n\nfull body", false, false)).unwrap();
    assert!(!cid.0.is_empty(), "commit 应返回非空 id");

    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].subject, "first commit");
    assert!(log[0].body.is_empty(), "历史列表不应预加载 commit 正文");
    assert!(log[0].author.email.is_empty(), "历史列表不应预加载作者邮箱");
    assert!(log[0].committer.name.is_empty(), "历史列表不应预加载提交者");
    let details = block_on(driver.commit_details(&id, &cid.0)).unwrap();
    assert_eq!(details.subject, "first commit");
    assert_eq!(details.body, "full body");
    assert_eq!(details.author.email, "test@ramag.dev");

    let st = block_on(driver.status(&id)).unwrap();
    assert!(st.files.is_empty(), "commit 后工作区应干净");
    assert!(st.head_branch.is_some(), "应有 HEAD 分支");
}

#[test]
fn log_pages_continue_one_stream_and_reset_for_new_query() {
    let (driver, id, tmp) = setup();
    for index in 0..5 {
        commit_file(
            &driver,
            &id,
            tmp.path(),
            "a.txt",
            &format!("content {index}\n"),
            &format!("commit {index}"),
        );
    }

    let page = |skip, grep| {
        block_on(driver.log(
            &id,
            LogOptions {
                start: Some("HEAD".into()),
                skip,
                limit: Some(2),
                grep,
                ..Default::default()
            },
        ))
        .unwrap()
    };
    assert_eq!(
        page(0, None)
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>(),
        ["commit 4", "commit 3"]
    );
    assert_eq!(
        page(2, None)
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>(),
        ["commit 2", "commit 1"]
    );
    assert_eq!(page(4, None)[0].subject, "commit 0");

    let filtered = page(0, Some("commit 3".into()));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].subject, "commit 3");
}

#[test]
fn path_scoped_status_returns_only_requested_changes() {
    let (driver, id, tmp) = setup();
    write(tmp.path(), "a.txt", "base a\n");
    write(tmp.path(), "b.txt", "base b\n");
    block_on(driver.stage(&id, &["a.txt".into(), "b.txt".into()])).unwrap();
    block_on(driver.commit(&id, "base", false, false)).unwrap();

    write(tmp.path(), "a.txt", "changed a\n");
    write(tmp.path(), "b.txt", "changed b\n");
    let files = block_on(driver.status_paths(&id, &["a.txt".into()])).unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "a.txt");
    assert_eq!(files[0].unstaged, Some(FileChangeKind::Modified));
}

#[test]
fn path_scoped_project_files_returns_tracked_and_visible_untracked_members() {
    let (driver, id, tmp) = setup();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    write(tmp.path(), "src/a.rs", "tracked\n");
    write(tmp.path(), "src/b.rs", "untracked\n");
    write(tmp.path(), "outside.rs", "outside\n");
    block_on(driver.stage(&id, &["src/a.rs".into()])).unwrap();
    block_on(driver.commit(&id, "base", false, false)).unwrap();

    let files = block_on(driver.list_files_paths(&id, &["src".into()])).unwrap();

    assert_eq!(files, ["src/a.rs", "src/b.rs"]);
}

#[test]
fn empty_repo_log_is_normal_empty_state() {
    let (driver, id, _tmp) = setup();
    let log = block_on(driver.log(&id, LogOptions::default()))
        .expect("空仓库历史应返回空列表而不是 fatal");
    assert!(log.is_empty());
}

#[test]
fn unstage_and_discard() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");

    // 改 + stage，再 unstage
    write(tmp.path(), "a.txt", "base\nmore\n");
    block_on(driver.stage(&id, &["a.txt".to_string()])).unwrap();
    block_on(driver.unstage(&id, &["a.txt".to_string()])).unwrap();
    let st = block_on(driver.status(&id)).unwrap();
    let f = st.files.iter().find(|f| f.path == "a.txt").unwrap();
    assert!(f.staged.is_none(), "unstage 后不应 staged");
    assert!(f.unstaged.is_some(), "改动仍在工作区");

    // discard 丢弃工作区改动
    block_on(driver.discard(&id, &["a.txt".to_string()])).unwrap();
    let st = block_on(driver.status(&id)).unwrap();
    assert!(st.files.is_empty(), "discard 后工作区应干净");
}

#[test]
fn pathspec_stdin_treats_special_file_names_literally() {
    let (driver, id, tmp) = setup();
    let mut names = vec!["-leading.txt", "!literal.txt"];
    #[cfg(unix)]
    names.extend([":(glob)*.txt", "line\nbreak.txt"]);

    for name in &names {
        write(tmp.path(), name, "base\n");
    }
    let paths = names
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    block_on(driver.stage(&id, &paths)).expect("特殊文件名应按字面量暂存");
    let staged = block_on(driver.status(&id)).unwrap();
    assert!(names.iter().all(|name| {
        staged
            .files
            .iter()
            .any(|file| file.path == *name && file.staged.is_some())
    }));

    block_on(driver.unstage(&id, &paths)).expect("特殊文件名应按字面量撤回暂存");
    block_on(driver.stage(&id, &paths)).unwrap();
    block_on(driver.commit(&id, "special paths", false, false)).unwrap();
    for name in &names {
        write(tmp.path(), name, "changed\n");
    }
    block_on(driver.discard(&id, &paths)).expect("特殊文件名应按字面量丢弃改动");
    for name in &names {
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(name)).unwrap(),
            "base\n"
        );
    }
}

#[test]
fn unstage_before_first_commit_keeps_worktree_file() {
    let (driver, id, tmp) = setup();
    write(tmp.path(), "first.txt", "draft\n");
    block_on(driver.stage(&id, &["first.txt".to_string()])).unwrap();

    block_on(driver.unstage(&id, &["first.txt".to_string()]))
        .expect("首次 commit 前也应能取消暂存");

    let status = block_on(driver.status(&id)).unwrap();
    let file = status
        .files
        .iter()
        .find(|file| file.path == "first.txt")
        .expect("工作区文件不应被删除");
    assert!(file.staged.is_none());
    assert!(matches!(file.unstaged, Some(FileChangeKind::Untracked)));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("first.txt")).unwrap(),
        "draft\n"
    );
}

#[test]
fn hunk_unstage_before_first_commit_keeps_worktree_file() {
    let (driver, id, tmp) = setup();
    write(tmp.path(), "first.txt", "draft\n");
    block_on(driver.stage(&id, &["first.txt".to_string()])).unwrap();
    let patch = "diff --git a/first.txt b/first.txt\n--- /dev/null\n+++ b/first.txt\n@@ -0,0 +1,1 @@\n+draft\n";

    block_on(driver.unstage_patch(&id, patch)).expect("首次 commit 前也应能按 hunk 取消暂存");

    let status = block_on(driver.status(&id)).unwrap();
    let file = status
        .files
        .iter()
        .find(|file| file.path == "first.txt")
        .expect("工作区文件不应被删除");
    assert!(file.staged.is_none());
    assert!(matches!(file.unstaged, Some(FileChangeKind::Untracked)));
}

/// 行级部分暂存：只 stage 选中的新增行，其余改动留在工作区
#[test]
fn line_level_partial_stage() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "a\nb\nc\n", "init");

    // 加 X（b 前）和 Y（末尾）
    write(tmp.path(), "a.txt", "a\nX\nb\nc\nY\n");

    // 只 stage X 的行级 patch（Y 不 stage）—— build_patch_for_selection 的输出格式：
    // 选中的 add 保留 +，未选中的 add 省略，context 保留，真实 old_start 定位
    let patch =
        "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n a\n+X\n b\n c\n";
    block_on(driver.stage_patch(&id, patch)).expect("行级 stage_patch 应成功");

    let st = block_on(driver.status(&id)).unwrap();
    let f = st.files.iter().find(|f| f.path == "a.txt").unwrap();
    assert!(f.staged.is_some(), "X 应进暂存区");
    assert!(f.unstaged.is_some(), "Y 应还在工作区未暂存");

    // index 内容应为 a\nX\nb\nc\n（含 X、不含 Y）
    let staged_diff = block_on(driver.diff_file(&id, "a.txt", DiffKind::IndexVsHead)).unwrap();
    let added: Vec<&str> = staged_diff
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| matches!(l.kind, ramag_domain::entities::DiffLineKind::Add))
        .map(|l| l.text.as_str())
        .collect();
    assert!(added.contains(&"X"), "暂存区应含 X，实际 {added:?}");
    assert!(!added.contains(&"Y"), "暂存区不应含 Y，实际 {added:?}");
}

/// 行级 unstage：从暂存区撤回选中行
#[test]
fn line_level_partial_unstage() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "a\nb\nc\n", "init");
    // 全量改 + stage
    write(tmp.path(), "a.txt", "a\nX\nb\nc\n");
    block_on(driver.stage(&id, &["a.txt".to_string()])).unwrap();
    // 行级 unstage X
    let patch =
        "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n a\n+X\n b\n c\n";
    block_on(driver.unstage_patch(&id, patch)).expect("行级 unstage_patch 应成功");
    let st = block_on(driver.status(&id)).unwrap();
    let f = st.files.iter().find(|f| f.path == "a.txt").unwrap();
    assert!(f.unstaged.is_some(), "撤回后 X 回到工作区未暂存");
}
