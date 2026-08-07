use super::*;

pub(super) async fn fetch(driver: &GitDriverImpl, repo: &RepoId, remote: &str) -> Result<()> {
    if !remote.is_empty() {
        git_cmd::validate_name_arg(remote, "远程名")?;
    }
    let handle = driver.get_repo(repo)?;
    let remote = remote.to_string();
    run_write_blocking(handle, move |p| remote::fetch(p, &remote)).await
}

pub(super) async fn push(
    driver: &GitDriverImpl,
    repo: &RepoId,
    remote: &str,
    branch: &str,
    set_upstream: bool,
    force_with_lease: bool,
) -> Result<()> {
    git_cmd::validate_name_arg(remote, "远程名")?;
    git_cmd::validate_name_arg(branch, "分支名")?;
    let handle = driver.get_repo(repo)?;
    let remote = remote.to_string();
    let branch = branch.to_string();
    run_write_blocking(handle, move |p| {
        remote::push(p, &remote, &branch, set_upstream, force_with_lease)
    })
    .await
}

pub(super) async fn pull(
    driver: &GitDriverImpl,
    repo: &RepoId,
    remote: &str,
    branch: &str,
    rebase: bool,
) -> Result<()> {
    git_cmd::validate_name_arg(remote, "远程名")?;
    git_cmd::validate_name_arg(branch, "分支名")?;
    let handle = driver.get_repo(repo)?;
    let remote = remote.to_string();
    let branch = branch.to_string();
    run_write_blocking(handle, move |p| remote::pull(p, &remote, &branch, rebase)).await
}

pub(super) async fn fetch_streaming(
    driver: &GitDriverImpl,
    repo: &RepoId,
    remote: &str,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: std::sync::Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    if !remote.is_empty() {
        git_cmd::validate_name_arg(remote, "远程名")?;
    }
    let handle = driver.get_repo(repo)?;
    let remote = remote.to_string();
    run_write_blocking(handle, move |p| {
        remote::fetch_streaming(p, &remote, cancel, progress)
    })
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn push_streaming(
    driver: &GitDriverImpl,
    repo: &RepoId,
    remote: &str,
    branch: &str,
    set_upstream: bool,
    force_with_lease: bool,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: std::sync::Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    git_cmd::validate_name_arg(remote, "远程名")?;
    git_cmd::validate_name_arg(branch, "分支名")?;
    let handle = driver.get_repo(repo)?;
    let remote = remote.to_string();
    let branch = branch.to_string();
    run_write_blocking(handle, move |p| {
        remote::push_streaming(
            p,
            &remote,
            &branch,
            set_upstream,
            force_with_lease,
            cancel,
            progress,
        )
    })
    .await
}

pub(super) async fn pull_streaming(
    driver: &GitDriverImpl,
    repo: &RepoId,
    remote: &str,
    branch: &str,
    rebase: bool,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: std::sync::Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    git_cmd::validate_name_arg(remote, "远程名")?;
    git_cmd::validate_name_arg(branch, "分支名")?;
    let handle = driver.get_repo(repo)?;
    let remote = remote.to_string();
    let branch = branch.to_string();
    run_write_blocking(handle, move |p| {
        remote::pull_streaming(p, &remote, &branch, rebase, cancel, progress)
    })
    .await
}
