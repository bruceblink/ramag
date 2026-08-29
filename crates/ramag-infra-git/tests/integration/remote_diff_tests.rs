use super::*;

#[test]
fn remote_add_list() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    block_on(driver.add_remote(&id, "origin", "https://example.com/r.git")).unwrap();
    let remotes = block_on(driver.list_remotes(&id)).unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "origin");
}

#[test]
fn remote_rename_set_url_remove() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    block_on(driver.add_remote(&id, "origin", "https://example.com/r.git")).unwrap();

    block_on(driver.rename_remote(&id, "origin", "upstream")).unwrap();
    let remotes = block_on(driver.list_remotes(&id)).unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "upstream", "重命名后应为 upstream");

    block_on(driver.set_remote_url(&id, "upstream", "https://example.com/new.git")).unwrap();
    let remotes = block_on(driver.list_remotes(&id)).unwrap();
    assert_eq!(
        remotes[0].fetch_url, "https://example.com/new.git",
        "fetch URL 应已更新"
    );

    block_on(driver.remove_remote(&id, "upstream")).unwrap();
    let remotes = block_on(driver.list_remotes(&id)).unwrap();
    assert!(remotes.is_empty(), "删除后应无 remote");
}

#[test]
fn reflog_records_commits() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "v1\n", "c1");
    commit_file(&driver, &id, tmp.path(), "a.txt", "v2\n", "c2");
    let reflog = block_on(driver.list_reflog(&id, None, Some(50))).unwrap();
    assert!(
        reflog.len() >= 2,
        "reflog 应记录 commit 操作，实际 {}",
        reflog.len()
    );
}

#[test]
fn list_files_and_commit_files() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "x\n", "c1");
    write(tmp.path(), "b.txt", "y\n"); // untracked

    let files = block_on(driver.list_files(&id)).unwrap();
    assert!(files.contains(&"a.txt".to_string()), "应含 tracked a.txt");
    assert!(
        files.contains(&"b.txt".to_string()),
        "list_files 应含 untracked b.txt"
    );

    let cid = block_on(driver.log(&id, LogOptions::default())).unwrap()[0]
        .id
        .0
        .clone();
    let cf = block_on(driver.list_commit_files(&id, &cid)).unwrap();
    assert!(
        cf.iter().any(|f| f.path == "a.txt"),
        "commit 文件应含 a.txt"
    );
}

#[test]
fn list_range_files_reports_changes_and_renames() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "base.txt", "base\n", "init");
    let base = block_on(driver.status(&id)).unwrap().head_commit.unwrap();

    std::fs::rename(tmp.path().join("base.txt"), tmp.path().join("renamed.txt")).unwrap();
    write(tmp.path(), "changed.txt", "new\n");
    block_on(driver.stage(
        &id,
        &[
            "base.txt".into(),
            "renamed.txt".into(),
            "changed.txt".into(),
        ],
    ))
    .unwrap();
    let target = block_on(driver.commit(&id, "rename and add", false, false)).unwrap();

    let files = block_on(driver.list_diff_files(&id, &base, &target.0)).unwrap();
    assert!(files.iter().any(|file| {
        file.path == "renamed.txt"
            && file.old_path.as_deref() == Some("base.txt")
            && file.staged == Some(FileChangeKind::Renamed)
    }));
    assert!(
        files.iter().any(|file| {
            file.path == "changed.txt" && file.staged == Some(FileChangeKind::Added)
        })
    );
}

/// diff_file 内容 + 行号映射精确正确（diff 渲染的输入，保证 UI 不会行错位）
#[test]
fn diff_content_and_line_numbers_precise() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "a\nb\nc\n", "init");
    write(tmp.path(), "a.txt", "a\nB\nc\n"); // b → B

    let diff = block_on(driver.diff_file(&id, "a.txt", DiffKind::WorkingTreeVsIndex)).unwrap();
    let lines: Vec<&ramag_domain::entities::DiffLine> =
        diff.hunks.iter().flat_map(|h| &h.lines).collect();

    let del = lines.iter().find(|l| l.text == "b").expect("应有删除行 b");
    assert!(matches!(del.kind, DiffLineKind::Delete), "b 应为 Delete");
    assert!(
        del.old_lineno == Some(2) && del.new_lineno.is_none(),
        "删除行：有 old_lineno(2) 无 new_lineno，实际 old={:?} new={:?}",
        del.old_lineno,
        del.new_lineno
    );

    let add = lines.iter().find(|l| l.text == "B").expect("应有新增行 B");
    assert!(matches!(add.kind, DiffLineKind::Add), "B 应为 Add");
    assert!(
        add.new_lineno == Some(2) && add.old_lineno.is_none(),
        "新增行：有 new_lineno(2) 无 old_lineno，实际 old={:?} new={:?}",
        add.old_lineno,
        add.new_lineno
    );

    // context 行 a/c 两侧行号都在
    let ctx_a = lines
        .iter()
        .find(|l| l.text == "a")
        .expect("应有 context a");
    assert!(
        ctx_a.old_lineno == Some(1) && ctx_a.new_lineno == Some(1),
        "context a 两侧行号应为 1"
    );
}

fn adds_of(diff: &ramag_domain::entities::FileDiff) -> Vec<String> {
    diff.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, DiffLineKind::Add))
        .map(|l| l.text.clone())
        .collect()
}

fn dels_of(diff: &ramag_domain::entities::FileDiff) -> Vec<String> {
    diff.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, DiffLineKind::Delete))
        .map(|l| l.text.clone())
        .collect()
}

/// 根 commit 的文件 diff：之前 `git diff <c>^ <c>` 对无父 commit 报错，点第一个 commit 看 diff 失败
#[test]
fn diff_root_commit_file() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "l1\nl2\nl3\n", "root");
    let cid = block_on(driver.log(&id, LogOptions::default())).unwrap()[0]
        .id
        .0
        .clone();

    let diff = block_on(driver.diff_file(&id, "a.txt", DiffKind::CommitVsParent(CommitId(cid))))
        .expect("根 commit 的文件 diff 应成功（不再因 <c>^ 不存在而报错）");
    assert_eq!(
        adds_of(&diff),
        vec!["l1", "l2", "l3"],
        "根 commit diff 应把所有行显示为新增"
    );
}

/// 普通（有父）commit 的文件 diff
#[test]
fn diff_normal_commit_file() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "v1\n", "c1");
    commit_file(&driver, &id, tmp.path(), "a.txt", "v2\n", "c2");
    let cid = block_on(driver.log(&id, LogOptions::default())).unwrap()[0]
        .id
        .0
        .clone();

    let diff =
        block_on(driver.diff_file(&id, "a.txt", DiffKind::CommitVsParent(CommitId(cid)))).unwrap();
    assert_eq!(dels_of(&diff), vec!["v1"], "c2 应删 v1");
    assert_eq!(adds_of(&diff), vec!["v2"], "c2 应增 v2");
}

/// 大文件首尾两处改动 → 应分成 2 个 hunk，行号连续正确（验证多 hunk 解析）
#[test]
fn diff_large_file_multi_hunk() {
    let (driver, id, tmp) = setup();
    let content: String = (1..=50).map(|i| format!("line{i}\n")).collect();
    commit_file(&driver, &id, tmp.path(), "big.txt", &content, "init");

    let mut lines: Vec<String> = (1..=50).map(|i| format!("line{i}")).collect();
    lines[1] = "LINE2".into(); // 改第 2 行
    lines[48] = "LINE49".into(); // 改第 49 行
    write(tmp.path(), "big.txt", &(lines.join("\n") + "\n"));

    let diff = block_on(driver.diff_file(&id, "big.txt", DiffKind::WorkingTreeVsIndex)).unwrap();
    assert_eq!(diff.hunks.len(), 2, "相距很远的两处改动应分成 2 个 hunk");
    assert!(
        diff.hunks[1].old_start > 40,
        "第二个 hunk 行号应接近 49，实际 {}",
        diff.hunks[1].old_start
    );
    assert!(adds_of(&diff).contains(&"LINE49".to_string()));
}

/// 纯新增文件（split 渲染会退化 unified）
#[test]
fn diff_pure_add_file() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "base\n", "init");
    write(tmp.path(), "new.txt", "n1\nn2\n");
    block_on(driver.stage(&id, &["new.txt".to_string()])).unwrap();

    let diff = block_on(driver.diff_file(&id, "new.txt", DiffKind::IndexVsHead)).unwrap();
    assert_eq!(diff.change_kind, FileChangeKind::Added);
    assert_eq!(adds_of(&diff), vec!["n1", "n2"]);
}

/// 无换行结尾文件：git diff 输出 `\ No newline at end of file`，解析须忽略该标记不当成内容行
#[test]
fn diff_no_newline_at_eof() {
    let (driver, id, tmp) = setup();
    commit_file(&driver, &id, tmp.path(), "a.txt", "l1\nl2\n", "init");
    std::fs::write(tmp.path().join("a.txt"), "l1\nl2\nl3").unwrap(); // 末尾无换行

    let diff = block_on(driver.diff_file(&id, "a.txt", DiffKind::WorkingTreeVsIndex)).unwrap();
    assert!(
        adds_of(&diff).contains(&"l3".to_string()),
        "应识别新增行 l3"
    );
    let all: Vec<String> = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l| l.text.clone())
        .collect();
    assert!(
        !all.iter().any(|t| t.contains("No newline")),
        "`\\ No newline` 标记不应被当成内容行，实际 {all:?}"
    );
}

/// amend：空 message 保留原 commit message；非空 message 改写。两种都不应新增 commit 数
#[test]
fn amend_keeps_or_rewrites_message() {
    let (driver, id, tmp) = setup();
    commit_file(
        &driver,
        &id,
        tmp.path(),
        "a.txt",
        "v1\n",
        "original message",
    );

    // 空 message amend：补一个文件进上一个 commit，message 不变
    write(tmp.path(), "b.txt", "extra\n");
    block_on(driver.stage(&id, &["b.txt".to_string()])).unwrap();
    block_on(driver.commit(&id, "", true, false)).expect("空 message amend 应成功");
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log.len(), 1, "amend 不应新增 commit");
    assert_eq!(log[0].subject, "original message", "空 message 应保留原文");

    // 非空 message amend：改写 message
    block_on(driver.commit(&id, "rewritten", true, false)).expect("amend 改 message 应成功");
    let log = block_on(driver.log(&id, LogOptions::default())).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].subject, "rewritten");
}
