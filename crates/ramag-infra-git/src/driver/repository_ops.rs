use super::*;

pub(super) async fn list_stashes(driver: &GitDriverImpl, repo: &RepoId) -> Result<Vec<Stash>> {
    let handle = driver.get_repo(repo)?;
    run_blocking(move || stash::list(&handle.path)).await
}

pub(super) async fn stash_save(
    driver: &GitDriverImpl,
    repo: &RepoId,
    message: Option<&str>,
    include_untracked: bool,
) -> Result<()> {
    if let Some(message) = message {
        stash::validate_message(message)?;
    }
    let handle = driver.get_repo(repo)?;
    let msg = message.map(str::to_owned);
    run_write_blocking(handle, move |p| {
        stash::save(p, msg.as_deref(), include_untracked)
    })
    .await
}

pub(super) async fn stash_apply(
    driver: &GitDriverImpl,
    repo: &RepoId,
    idx: usize,
    pop: bool,
) -> Result<()> {
    let handle = driver.get_repo(repo)?;
    run_write_blocking(handle, move |p| stash::apply(p, idx, pop)).await
}

pub(super) async fn stash_drop(driver: &GitDriverImpl, repo: &RepoId, idx: usize) -> Result<()> {
    let handle = driver.get_repo(repo)?;
    run_write_blocking(handle, move |p| stash::drop(p, idx)).await
}

pub(super) async fn list_tags(driver: &GitDriverImpl, repo: &RepoId) -> Result<Vec<Tag>> {
    let handle = driver.get_repo(repo)?;
    run_blocking(move || tag::list(&handle.path)).await
}

pub(super) async fn create_tag(
    driver: &GitDriverImpl,
    repo: &RepoId,
    name: &str,
    target: Option<&str>,
    message: Option<&str>,
    sign: bool,
) -> Result<()> {
    git_cmd::validate_name_arg(name, "tag 名")?;
    if let Some(target) = target {
        git_cmd::validate_positional_arg(target, "tag 目标")?;
    }
    if let Some(message) = message {
        tag::validate_message(message)?;
    }
    let handle = driver.get_repo(repo)?;
    let name = name.to_string();
    let target = target.map(str::to_owned);
    let message = message.map(str::to_owned);
    run_write_blocking(handle, move |p| {
        tag::create(p, &name, target.as_deref(), message.as_deref(), sign)
    })
    .await
}

pub(super) async fn delete_tag(driver: &GitDriverImpl, repo: &RepoId, name: &str) -> Result<()> {
    git_cmd::validate_name_arg(name, "tag 名")?;
    let handle = driver.get_repo(repo)?;
    let name = name.to_string();
    run_write_blocking(handle, move |p| tag::delete(p, &name)).await
}

pub(super) async fn push_tag(
    driver: &GitDriverImpl,
    repo: &RepoId,
    remote: &str,
    name: &str,
) -> Result<()> {
    git_cmd::validate_name_arg(remote, "远程名")?;
    git_cmd::validate_name_arg(name, "tag 名")?;
    let handle = driver.get_repo(repo)?;
    let remote = remote.to_string();
    let name = name.to_string();
    run_write_blocking(handle, move |p| tag::push(p, &remote, &name)).await
}
