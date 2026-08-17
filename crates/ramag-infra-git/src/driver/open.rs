use super::*;

pub(super) async fn open_repo(driver: &GitDriverImpl, path: &Path) -> Result<RepoConfig> {
    // dunce 将 Windows 扩展路径还原为常规路径。
    let canonical = dunce::canonicalize(path)
        .map_err(|e| DomainError::InvalidConfig(format!("路径无法访问: {e}")))?;

    // 同一路径的首次打开串行化。
    let open_lock = driver.open_lock(&canonical);
    let _guard = open_lock.lock().await;

    // 复用存活句柄，清理失效映射。
    if let Some(existing_id) = driver.by_path.get(&canonical) {
        let id = existing_id.clone();
        drop(existing_id);
        if driver.repos.contains_key(&id) {
            let path_string = canonical.to_string_lossy().into_owned();
            return Ok(RepoConfig::from_path(path_string).with_id(id));
        }
        driver.by_path.remove(&canonical);
    }

    let canonical_for_open = canonical.clone();
    let repo = run_blocking(move || gix::open(&canonical_for_open).map_err(errors::map_open_error))
        .await?;

    let git_dir = repo.git_dir().to_path_buf();
    let id = RepoId::new();
    let handle = Arc::new(OpenRepo {
        path: canonical.clone(),
        git_dir,
        write_lock: Arc::new(parking_lot::Mutex::new(())),
        log_pager: parking_lot::Mutex::new(None),
    });
    driver.repos.insert(id.clone(), handle);
    driver.by_path.insert(canonical.clone(), id.clone());

    let path_string = canonical.to_string_lossy().into_owned();
    Ok(RepoConfig::from_path(path_string).with_id(id))
}
