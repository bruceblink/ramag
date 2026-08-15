//! Git 历史查询与分页。

use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};
use std::thread::JoinHandle;

use ramag_domain::entities::{Commit, LogOptions};
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

use crate::git_cmd::{
    LimitedBytes, MAX_GIT_MESSAGE_BYTES, MAX_STDERR_BYTES, MAX_STDOUT_BYTES, ensure_git_list_room,
    ensure_git_message_size, machine_command, read_limited, run_git_probe, run_git_text,
    validate_path_arg, validate_positional_arg,
};

/// 列表只读取摘要；详情按需读取完整字段。
const LOG_LIST_FORMAT: &str = "%H%x1f%an%x1f%at%x1f%P%x1f%D%x1f%s%x1e";
const LOG_DETAIL_FORMAT: &str =
    "%H%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ce%x1f%ct%x1f%P%x1f%D%x1f%s%x1f%b%x1e";
mod parse;

use parse::{parse_log_list_output, parse_log_list_record, parse_log_output};

pub(crate) type LogPagerSlot = parking_lot::Mutex<Option<LogPager>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogQueryKey {
    start: Option<String>,
    path_filter: Option<String>,
    current_branch_only: bool,
    grep: Option<String>,
    author: Option<String>,
    since: Option<String>,
}

impl From<&LogOptions> for LogQueryKey {
    fn from(options: &LogOptions) -> Self {
        Self {
            start: options.start.clone(),
            path_filter: options.path_filter.clone(),
            current_branch_only: options.current_branch_only,
            grep: options.grep.clone(),
            author: options.author.clone(),
            since: options.since.clone(),
        }
    }
}

pub(crate) struct LogPager {
    key: LogQueryKey,
    offset: usize,
    child: Child,
    stdout: std::io::BufReader<ChildStdout>,
    stderr_reader: Option<JoinHandle<std::io::Result<LimitedBytes>>>,
    finished: bool,
}

pub fn run_log(repo_path: &Path, opts: &LogOptions) -> Result<Vec<Commit>> {
    validate_log_options(opts)?;
    if !has_log_start(repo_path, opts)? {
        return Ok(Vec::new());
    }
    let args = build_log_args(opts, true);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_git_text(repo_path, &args_ref)?;
    parse_log_list_output(&out).map_err(|error| {
        warn!(
            operation = "git_log_parse",
            repo = %repo_path.display(),
            mode = "list",
            skip = opts.skip,
            limit = ?opts.limit,
            error = %error,
            "git log output parse failed"
        );
        error
    })
}

/// 连续翻页复用同一进程；查询或偏移变化时回退一次性读取。
pub(crate) fn run_log_paged(
    repo_path: &Path,
    slot: &LogPagerSlot,
    opts: &LogOptions,
) -> Result<Vec<Commit>> {
    validate_log_options(opts)?;
    let Some(limit) = opts.limit else {
        return run_log(repo_path, opts);
    };
    if limit == 0 || !has_log_start(repo_path, opts)? {
        return Ok(Vec::new());
    }

    let key = LogQueryKey::from(opts);
    let mut current = slot.lock();
    if opts.skip == 0 {
        current.take();
        let mut pager = LogPager::spawn(repo_path, opts, key).map_err(|error| {
            log_page_error(repo_path, opts, &error);
            error
        })?;
        let result = pager.read_page(limit);
        return match result {
            Ok((commits, finished)) => {
                if !finished {
                    *current = Some(pager);
                }
                Ok(commits)
            }
            Err(error) => {
                log_page_error(repo_path, opts, &error);
                Err(error)
            }
        };
    }

    let resumable = current
        .as_ref()
        .is_some_and(|pager| pager.key == key && pager.offset == opts.skip);
    if !resumable {
        // 查询或偏移变化时停止旧进程。
        current.take();
        drop(current);
        return run_log(repo_path, opts);
    }
    let result = match current.as_mut() {
        Some(pager) => pager.read_page(limit),
        None => {
            let error = DomainError::Other("Git 日志分页状态丢失".into());
            log_page_error(repo_path, opts, &error);
            return Err(error);
        }
    };
    match result {
        Ok((commits, finished)) => {
            if finished {
                current.take();
            }
            Ok(commits)
        }
        Err(error) => {
            current.take();
            log_page_error(repo_path, opts, &error);
            Err(error)
        }
    }
}

fn log_page_error(repo_path: &Path, opts: &LogOptions, error: &DomainError) {
    warn!(
        operation = "git_log_page",
        repo = %repo_path.display(),
        skip = opts.skip,
        limit = ?opts.limit,
        error = %error,
        "git log page failed"
    );
}

fn validate_log_options(opts: &LogOptions) -> Result<()> {
    if let Some(start) = &opts.start {
        validate_positional_arg(start, "日志起点")?;
    }
    if let Some(path) = &opts.path_filter {
        validate_path_arg(path, "日志文件路径")?;
    }
    for (label, value) in [
        ("日志关键词", opts.grep.as_deref()),
        ("日志作者", opts.author.as_deref()),
        ("日志时间范围", opts.since.as_deref()),
    ] {
        if let Some(value) = value {
            validate_log_filter(value, label)?;
        }
    }
    if opts
        .limit
        .is_some_and(|limit| limit > crate::git_cmd::MAX_PARSED_GIT_ITEMS)
    {
        return Err(DomainError::InvalidConfig(format!(
            "日志单页数量超过 {} 条安全上限",
            crate::git_cmd::MAX_PARSED_GIT_ITEMS
        )));
    }
    Ok(())
}

fn has_log_start(repo_path: &Path, opts: &LogOptions) -> Result<bool> {
    // 新仓库没有 HEAD 是正常空态。
    if opts.start.is_none()
        && !run_git_probe(repo_path, &["rev-parse", "--verify", "--quiet", "HEAD"])?
    {
        return Ok(false);
    }
    Ok(true)
}

fn build_log_args(opts: &LogOptions, include_page: bool) -> Vec<String> {
    let mut args: Vec<String> = vec!["log".into(), format!("--pretty=format:{LOG_LIST_FORMAT}")];
    if include_page {
        if opts.skip > 0 {
            args.push(format!("--skip={}", opts.skip));
        }
        if let Some(n) = opts.limit {
            args.push(format!("--max-count={n}"));
        }
    }
    if let Some(g) = &opts.grep {
        args.push(format!("--grep={g}"));
        // UI 搜索忽略大小写。
        args.push("--regexp-ignore-case".into());
    }
    if let Some(a) = &opts.author {
        args.push(format!("--author={a}"));
    }
    if let Some(s) = &opts.since {
        args.push(format!("--since={s}"));
    }
    if let Some(start) = &opts.start {
        args.push(start.clone());
    }
    if let Some(p) = &opts.path_filter {
        args.push("--".into());
        args.push(p.clone());
    }
    args
}

impl LogPager {
    fn spawn(repo_path: &Path, options: &LogOptions, key: LogQueryKey) -> Result<Self> {
        let args = build_log_args(options, false);
        let mut command = machine_command(repo_path);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| DomainError::QueryFailed(format!("启动流式 git log 失败：{error}")))?;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_pager_child(&mut child);
                return Err(DomainError::QueryFailed(
                    "无法读取流式 git log 标准输出".into(),
                ));
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_pager_child(&mut child);
                return Err(DomainError::QueryFailed(
                    "无法读取流式 git log 错误输出".into(),
                ));
            }
        };
        let stderr_reader = match std::thread::Builder::new()
            .name("ramag-git-log-stderr".into())
            .spawn(move || read_limited(stderr, MAX_STDERR_BYTES))
        {
            Ok(reader) => reader,
            Err(error) => {
                terminate_pager_child(&mut child);
                return Err(DomainError::QueryFailed(format!(
                    "启动流式 git log 错误读取线程失败：{error}"
                )));
            }
        };
        Ok(Self {
            key,
            offset: 0,
            child,
            stdout: std::io::BufReader::new(stdout),
            stderr_reader: Some(stderr_reader),
            finished: false,
        })
    }

    fn read_page(&mut self, limit: usize) -> Result<(Vec<Commit>, bool)> {
        let mut commits = Vec::with_capacity(limit.min(4_096));
        let mut page_bytes = 0usize;
        while commits.len() < limit {
            let Some(record) = read_log_record(&mut self.stdout)? else {
                self.finish()?;
                return Ok((commits, true));
            };
            page_bytes = page_bytes
                .checked_add(record.len())
                .ok_or_else(|| DomainError::QueryFailed("Git 日志分页输出大小溢出".into()))?;
            if page_bytes > MAX_STDOUT_BYTES {
                return Err(DomainError::QueryFailed(format!(
                    "git log 单页输出超过 {} MiB 安全上限",
                    MAX_STDOUT_BYTES / 1024 / 1024
                )));
            }
            let text = String::from_utf8_lossy(&record);
            let trimmed = text.trim_start_matches('\n');
            if trimmed.is_empty() {
                continue;
            }
            ensure_git_list_room(commits.len(), "Git 日志列表")?;
            ensure_git_message_size(trimmed.as_bytes(), "Git 日志记录", commits.len() + 1)?;
            commits.push(parse_log_list_record(trimmed).map_err(|reason| {
                DomainError::QueryFailed(format!(
                    "解析 Git 日志第 {} 条记录失败：{reason}",
                    self.offset + commits.len() + 1
                ))
            })?);
        }
        self.offset = self
            .offset
            .checked_add(commits.len())
            .ok_or_else(|| DomainError::QueryFailed("Git 日志分页 offset 溢出".into()))?;
        Ok((commits, false))
    }

    fn finish(&mut self) -> Result<()> {
        let status = self
            .child
            .wait()
            .map_err(|error| DomainError::QueryFailed(format!("等待流式 git log 失败：{error}")))?;
        self.finished = true;
        let mut stderr = self.join_stderr()?;
        if stderr.truncated {
            stderr
                .bytes
                .extend_from_slice(b"\n... git stderr truncated by Ramag");
        }
        if !status.success() {
            let detail = String::from_utf8_lossy(&stderr.bytes);
            return Err(DomainError::QueryFailed(crate::errors::friendly_git_error(
                &["log"],
                &detail,
            )));
        }
        Ok(())
    }

    fn join_stderr(&mut self) -> Result<LimitedBytes> {
        self.stderr_reader
            .take()
            .ok_or_else(|| DomainError::Other("Git 日志错误读取线程状态丢失".into()))?
            .join()
            .map_err(|_| DomainError::Other("Git 日志错误读取线程 panic".into()))?
            .map_err(|error| {
                DomainError::QueryFailed(format!("读取流式 git log 错误输出失败：{error}"))
            })
    }
}

impl Drop for LogPager {
    fn drop(&mut self) {
        if !self.finished {
            terminate_pager_child(&mut self.child);
        }
        if let Some(reader) = self.stderr_reader.take()
            && reader.join().is_err()
        {
            tracing::warn!(
                operation = "git_log_pager_cleanup",
                stage = "stderr_reader",
                "git log stderr reader panicked during cleanup"
            );
        }
    }
}

fn read_log_record(reader: &mut impl std::io::BufRead) -> Result<Option<Vec<u8>>> {
    let mut record = Vec::new();
    loop {
        let buffer = reader
            .fill_buf()
            .map_err(|error| DomainError::QueryFailed(format!("读取流式 git log 失败：{error}")))?;
        if buffer.is_empty() {
            return if record.is_empty() {
                Ok(None)
            } else {
                Err(DomainError::QueryFailed(
                    "流式 git log 记录缺少结束分隔符".into(),
                ))
            };
        }
        let delimiter = buffer.iter().position(|byte| *byte == b'\x1e');
        let take = delimiter.map_or(buffer.len(), |index| index + 1);
        if record.len().saturating_add(take) > MAX_GIT_MESSAGE_BYTES.saturating_add(1) {
            return Err(DomainError::QueryFailed(format!(
                "Git 日志记录超过 {} MiB 安全上限",
                MAX_GIT_MESSAGE_BYTES / 1024 / 1024
            )));
        }
        record.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if delimiter.is_some() {
            record.pop();
            return Ok(Some(record));
        }
    }
}

fn terminate_pager_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 按需读取一条完整 commit。
pub fn run_commit(repo_path: &Path, revision: &str) -> Result<Commit> {
    validate_positional_arg(revision, "commit 详情 revision")?;
    let format = format!("--format={LOG_DETAIL_FORMAT}");
    let out = run_git_text(
        repo_path,
        &["show", "--no-patch", "--no-notes", &format, revision],
    )?;
    let mut commits = parse_log_output(&out).map_err(|error| {
        warn!(
            operation = "git_commit_parse",
            repo = %repo_path.display(),
            revision = %revision,
            error = %error,
            "git commit detail output parse failed"
        );
        error
    })?;
    if commits.len() != 1 {
        let error = DomainError::QueryFailed(format!(
            "commit 详情应返回 1 条记录，实际 {} 条",
            commits.len()
        ));
        warn!(
            operation = "git_commit_parse",
            repo = %repo_path.display(),
            revision = %revision,
            record_count = commits.len(),
            error = %error,
            "git commit detail returned unexpected record count"
        );
        return Err(error);
    }
    commits
        .pop()
        .ok_or_else(|| DomainError::QueryFailed(format!("未找到 commit：{revision}")))
}

fn validate_log_filter(value: &str, label: &str) -> Result<()> {
    if value.len() > ramag_domain::entities::MAX_GIT_POSITIONAL_ARG_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidConfig(format!(
            "{label}超过 {} KiB 上限或包含控制字符",
            ramag_domain::entities::MAX_GIT_POSITIONAL_ARG_BYTES / 1024
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "log/tests.rs"]
mod tests;
