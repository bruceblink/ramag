//! 集成测试：对真实临时 git 仓库跑端到端操作，验证 Git 功能真实可用。
//! git 是本地命令，无需环境变量；缺 git 时 setup 会 panic。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use futures::executor::block_on;
use futures::future::join_all;
use ramag_domain::entities::{
    BranchKind, CommitId, DiffKind, DiffLineKind, FileChangeKind, LogOptions, RepoId, ResetKind,
};
use ramag_domain::traits::GitDriver;
use ramag_infra_git::GitDriverImpl;

/// 设 git 仓库级配置（commit 需要 user.name/email）
fn git_config(dir: &Path, key: &str, val: &str) {
    let ok = std::process::Command::new("git")
        .args(["-C", dir.to_str().unwrap(), "config", key, val])
        .status()
        .unwrap()
        .success();
    assert!(ok, "git config {key} 失败");
}

/// 建临时仓库 + 固定用户与换行配置，返回 (driver, repo_id, 临时目录守卫)
fn setup() -> (GitDriverImpl, RepoId, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let driver = GitDriverImpl::new();
    block_on(driver.init_repo(tmp.path())).expect("init_repo");
    git_config(tmp.path(), "user.email", "test@ramag.dev");
    git_config(tmp.path(), "user.name", "Ramag Test");
    git_config(tmp.path(), "commit.gpgsign", "false");
    git_config(tmp.path(), "core.autocrlf", "false");
    // open_repo 确保句柄注册（status 等按 RepoId 取句柄）
    let rc = block_on(driver.open_repo(tmp.path())).expect("open_repo");
    (driver, rc.id, tmp)
}

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

/// 写文件 + stage + commit 一条龙
fn commit_file(
    driver: &GitDriverImpl,
    id: &RepoId,
    dir: &Path,
    name: &str,
    content: &str,
    msg: &str,
) {
    write(dir, name, content);
    block_on(driver.stage(id, &[name.to_string()])).unwrap();
    block_on(driver.commit(id, msg, false, false)).unwrap();
}

/// 当前 HEAD 所在分支名（init 后默认分支名因 git 配置而异，动态取）
fn current_branch(driver: &GitDriverImpl, id: &RepoId) -> String {
    let branches = block_on(driver.list_branches(id, BranchKind::Local)).unwrap();
    branches
        .iter()
        .find(|b| b.is_head)
        .map(|b| b.name.clone())
        .expect("应有 HEAD 分支")
}

#[path = "integration/operation_tests.rs"]
mod operation_tests;
#[path = "integration/remote_diff_tests.rs"]
mod remote_diff_tests;
#[path = "integration/repository_tests.rs"]
mod repository_tests;
