//! 手动性能基准：默认忽略，通过环境变量指定真实仓库后运行。

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ramag_domain::entities::{DiffKind, LogOptions};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::GitDriver as _;
use ramag_infra_git::GitDriverImpl;

const DEFAULT_ITERATIONS: usize = 30;

#[test]
#[ignore = "需要 RAMAG_PERF_REPO 指向本地 Git 仓库"]
fn reports_workspace_refresh_latency() -> Result<()> {
    let repo_path = required_repo_path()?;
    let iterations = std::env::var("RAMAG_PERF_ITERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ITERATIONS);
    if iterations == 0 {
        return Err(DomainError::InvalidConfig(
            "RAMAG_PERF_ITERATIONS 必须大于 0".into(),
        ));
    }

    futures::executor::block_on(async {
        let driver = GitDriverImpl::new();
        let repo = driver.open_repo(&repo_path).await?;

        report_operation("workspace full refresh", iterations, || {
            refresh_workspace(&driver, &repo.id)
        })
        .await?;
        report_operation("status full", iterations, || async {
            Ok(driver.status(&repo.id).await?.files.len())
        })
        .await?;
        report_operation("branches combined", iterations, || async {
            let (local, remote) = driver.list_all_branches(&repo.id).await?;
            Ok(local.len() + remote.len())
        })
        .await?;
        report_operation("history page 100", iterations, || async {
            Ok(driver
                .log(
                    &repo.id,
                    LogOptions {
                        start: Some("HEAD".into()),
                        limit: Some(100),
                        ..Default::default()
                    },
                )
                .await?
                .len())
        })
        .await?;
        report_operation("history page 1000", iterations, || async {
            Ok(driver
                .log(
                    &repo.id,
                    LogOptions {
                        start: Some("HEAD".into()),
                        limit: Some(1_000),
                        ..Default::default()
                    },
                )
                .await?
                .len())
        })
        .await?;
        if let Some(pages) = std::env::var("RAMAG_PERF_HISTORY_PAGES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
        {
            report_operation("history repeated skip pages", iterations, || async {
                let mut commits = 0usize;
                for page in 0..pages {
                    commits = commits.saturating_add(
                        ramag_infra_git::log::run_log(
                            &repo_path,
                            &LogOptions {
                                start: Some("HEAD".into()),
                                skip: page.saturating_mul(1_000),
                                limit: Some(1_000),
                                ..Default::default()
                            },
                        )?
                        .len(),
                    );
                }
                Ok(commits)
            })
            .await?;
            report_operation("history stream pages", iterations, || async {
                let mut commits = 0usize;
                for page in 0..pages {
                    commits = commits.saturating_add(
                        driver
                            .log(
                                &repo.id,
                                LogOptions {
                                    start: Some("HEAD".into()),
                                    skip: page.saturating_mul(1_000),
                                    limit: Some(1_000),
                                    ..Default::default()
                                },
                            )
                            .await?
                            .len(),
                    );
                }
                Ok(commits)
            })
            .await?;
        }
        if let Some(skip) = std::env::var("RAMAG_PERF_HISTORY_SKIP")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
        {
            report_operation("history deep page 100", iterations, || async {
                Ok(driver
                    .log(
                        &repo.id,
                        LogOptions {
                            start: Some("HEAD".into()),
                            skip,
                            limit: Some(100),
                            ..Default::default()
                        },
                    )
                    .await?
                    .len())
            })
            .await?;
            report_operation("history deep page 1000", iterations, || async {
                Ok(driver
                    .log(
                        &repo.id,
                        LogOptions {
                            start: Some("HEAD".into()),
                            skip,
                            limit: Some(1_000),
                            ..Default::default()
                        },
                    )
                    .await?
                    .len())
            })
            .await?;
        }

        if let Some(path) = std::env::var_os("RAMAG_PERF_PATH") {
            let path = path.to_string_lossy().into_owned();
            report_operation("status path", iterations, || async {
                Ok(driver
                    .status_paths(&repo.id, std::slice::from_ref(&path))
                    .await?
                    .len())
            })
            .await?;
            report_operation("zed-equivalent rich path status", iterations, || async {
                run_zed_reference_status(&repo_path, &path)
            })
            .await?;
        }
        if let Some(path) = std::env::var_os("RAMAG_PERF_DIFF_PATH") {
            let path = path.to_string_lossy().into_owned();
            report_operation("diff standard", iterations, || async {
                let diff = driver
                    .diff_file_full_opts(&repo.id, &path, DiffKind::WorkingTreeVsIndex, false, 3)
                    .await?;
                Ok(diff.hunks.iter().map(|hunk| hunk.lines.len()).sum())
            })
            .await?;
            report_operation("diff full file", iterations, || async {
                let diff = driver
                    .diff_file_full_opts(
                        &repo.id,
                        &path,
                        DiffKind::WorkingTreeVsIndex,
                        false,
                        999_999,
                    )
                    .await?;
                Ok(diff.hunks.iter().map(|hunk| hunk.lines.len()).sum())
            })
            .await?;
        }
        if let Some(path) = std::env::var_os("RAMAG_PERF_FILE_PATH") {
            let path = path.to_string_lossy().into_owned();
            report_operation("project files full", iterations, || async {
                Ok(driver.list_files(&repo.id).await?.len())
            })
            .await?;
            report_operation("project files path", iterations, || async {
                Ok(driver
                    .list_files_paths(&repo.id, std::slice::from_ref(&path))
                    .await?
                    .len())
            })
            .await?;
        }
        driver.close_repo(&repo.id).await
    })
}

/// 当前 Zed 路径刷新并发执行 status 与三种 numstat；这里只计进程与输出，不计解析，
/// 因而是偏向 Zed 的保守对照下界。
fn run_zed_reference_status(repo_path: &std::path::Path, path: &str) -> Result<usize> {
    let commands = [
        vec![
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--no-renames",
            "-z",
            "--",
            path,
        ],
        vec!["diff", "--numstat", "--no-renames", "HEAD", "--", path],
        vec![
            "diff",
            "--numstat",
            "--no-renames",
            "--cached",
            "HEAD",
            "--",
            path,
        ],
        vec!["diff", "--numstat", "--no-renames", "--", path],
    ];
    let mut children = Vec::with_capacity(commands.len());
    for args in commands {
        let child = std::process::Command::new("git")
            .current_dir(repo_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("LC_ALL", "C")
            .args(["-c", "core.fsmonitor=false"])
            .args(["-c", "log.showSignature=false"])
            .args(["--no-optional-locks", "--no-pager"])
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| DomainError::QueryFailed(format!("启动 Zed 对照命令失败：{error}")))?;
        children.push(child);
    }

    let mut bytes = 0usize;
    for child in children {
        let output = child
            .wait_with_output()
            .map_err(|error| DomainError::QueryFailed(format!("等待 Zed 对照命令失败：{error}")))?;
        if !output.status.success() {
            return Err(DomainError::QueryFailed(format!(
                "Zed 对照命令失败：{}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        bytes = bytes.saturating_add(output.stdout.len());
    }
    Ok(bytes)
}

async fn report_operation<F, Fut>(name: &str, iterations: usize, mut operation: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<usize>>,
{
    // 预热 Git 索引、文件系统缓存与 Ramag worker pool。
    black_box(operation().await?);
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        black_box(operation().await?);
        samples.push(start.elapsed());
    }
    Summary::from_samples(samples).report(name, iterations);
    Ok(())
}

async fn refresh_workspace(
    driver: &GitDriverImpl,
    repo: &ramag_domain::entities::RepoId,
) -> Result<usize> {
    let (status, branches) =
        futures::future::join(driver.status(repo), driver.list_all_branches(repo)).await;
    let (local, remote) = branches?;
    Ok(status?.files.len() + local.len() + remote.len())
}

fn required_repo_path() -> Result<PathBuf> {
    let path = std::env::var_os("RAMAG_PERF_REPO")
        .map(PathBuf::from)
        .ok_or_else(|| DomainError::InvalidConfig("缺少 RAMAG_PERF_REPO".into()))?;
    if !path.is_dir() {
        return Err(DomainError::InvalidConfig(format!(
            "RAMAG_PERF_REPO 不是目录：{}",
            path.display()
        )));
    }
    Ok(path)
}

struct Summary {
    median: Duration,
    p95: Duration,
    min: Duration,
    max: Duration,
}

impl Summary {
    fn from_samples(mut samples: Vec<Duration>) -> Self {
        samples.sort_unstable();
        let last = samples.len() - 1;
        let p95_index = (last * 95).div_ceil(100);
        Self {
            median: samples[last / 2],
            p95: samples[p95_index],
            min: samples[0],
            max: samples[last],
        }
    }

    fn report(&self, name: &str, iterations: usize) {
        eprintln!(
            "vcs {name}: iterations={iterations}, median={:.3} ms, p95={:.3} ms, min={:.3} ms, max={:.3} ms",
            self.median.as_secs_f64() * 1_000.0,
            self.p95.as_secs_f64() * 1_000.0,
            self.min.as_secs_f64() * 1_000.0,
            self.max.as_secs_f64() * 1_000.0,
        );
    }
}
