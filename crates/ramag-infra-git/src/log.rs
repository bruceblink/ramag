//! `git log --pretty=format:...`：`\x1f` 分字段、`\x1e` 分记录。
//! 历史列表只读渲染所需摘要；详情再按需读取完整签名与正文。

use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};
use std::thread::JoinHandle;

use ramag_domain::entities::{Commit, CommitId, LogOptions, Signature};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    LimitedBytes, MAX_GIT_MESSAGE_BYTES, MAX_STDERR_BYTES, MAX_STDOUT_BYTES, ensure_git_list_room,
    ensure_git_message_size, machine_command, read_limited, run_git_probe, run_git_text,
    validate_path_arg, validate_positional_arg,
};

/// 历史列表不传 commit body；详情打开时再用 `LOG_DETAIL_FORMAT` 单条读取。
const LOG_LIST_FORMAT: &str = "%H%x1f%an%x1f%at%x1f%P%x1f%D%x1f%s%x1e";
const LOG_DETAIL_FORMAT: &str =
    "%H%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ce%x1f%ct%x1f%P%x1f%D%x1f%s%x1f%b%x1e";
/// 正常 octopus merge 远低于此值；异常父边数量会放大提交图的内存与渲染成本。
const MAX_COMMIT_PARENTS: usize = 1024;
/// 引用只是装饰标签，超限时保留有界前缀并给出省略提示，不阻断历史浏览。
const MAX_COMMIT_REFS: usize = 256;

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
    parse_log_list_output(&out)
}

/// UI 连续翻页复用同一个 `git log` 进程；查询或 offset 不连续时安全退回一次性读取。
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
        let mut pager = LogPager::spawn(repo_path, opts, key)?;
        let result = pager.read_page(limit);
        return match result {
            Ok((commits, finished)) => {
                if !finished {
                    *current = Some(pager);
                }
                Ok(commits)
            }
            Err(error) => Err(error),
        };
    }

    let resumable = current
        .as_ref()
        .is_some_and(|pager| pager.key == key && pager.offset == opts.skip);
    if !resumable {
        // 查询或 offset 跳变后旧流已不可能安全复用，立即终止，避免仓库长期保留闲置子进程。
        current.take();
        drop(current);
        return run_log(repo_path, opts);
    }
    let result = current
        .as_mut()
        .ok_or_else(|| DomainError::Other("Git 日志分页状态丢失".into()))?
        .read_page(limit);
    match result {
        Ok((commits, finished)) => {
            if finished {
                current.take();
            }
            Ok(commits)
        }
        Err(error) => {
            current.take();
            Err(error)
        }
    }
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
    // 新初始化仓库没有 HEAD；这是正常空态，不应把 git log 的 fatal 暴露给用户。
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
        // git log 默认对 --grep 大小写敏感，UI 期望忽略
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
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
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

/// 详情页按需读取一条完整 commit，避免历史分页携带所有正文。
pub fn run_commit(repo_path: &Path, revision: &str) -> Result<Commit> {
    validate_positional_arg(revision, "commit 详情 revision")?;
    let format = format!("--format={LOG_DETAIL_FORMAT}");
    let out = run_git_text(
        repo_path,
        &["show", "--no-patch", "--no-notes", &format, revision],
    )?;
    let mut commits = parse_log_output(&out)?;
    if commits.len() != 1 {
        return Err(DomainError::QueryFailed(format!(
            "commit 详情应返回 1 条记录，实际 {} 条",
            commits.len()
        )));
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

fn parse_log_list_output(text: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::new();
    for (index, record) in text.split('\x1e').enumerate() {
        let trimmed = record.trim_start_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        ensure_git_list_room(commits.len(), "Git 日志列表")?;
        ensure_git_message_size(trimmed.as_bytes(), "Git 日志记录", index + 1)?;
        let commit = parse_log_list_record(trimmed).map_err(|reason| {
            DomainError::QueryFailed(format!(
                "解析 Git 日志第 {} 条记录失败：{reason}",
                index + 1
            ))
        })?;
        commits.push(commit);
    }
    Ok(commits)
}

fn parse_log_list_record(record: &str) -> std::result::Result<Commit, String> {
    let mut fields = record.splitn(6, '\x1f');
    let hash = fields.next().ok_or("缺少 commit id")?.trim();
    if hash.is_empty() {
        return Err("commit id 为空".into());
    }
    let author_name = fields.next().ok_or("缺少作者名")?;
    let author_ts = fields
        .next()
        .ok_or("缺少作者时间")?
        .parse::<i64>()
        .map_err(|error| format!("作者时间非整数：{error}"))?;
    let parents = parse_parents(fields.next().ok_or("缺少父提交字段")?)?;
    let refs = parse_refs(fields.next().ok_or("缺少引用字段")?);
    let subject = fields.next().ok_or("缺少提交主题")?.to_string();
    let author_timestamp = chrono::DateTime::from_timestamp(author_ts, 0)
        .ok_or_else(|| format!("作者时间超出支持范围：{author_ts}"))?;

    Ok(Commit {
        id: CommitId(hash.to_string()),
        parents,
        author: Signature {
            name: author_name.to_string(),
            email: String::new(),
            timestamp: author_timestamp,
        },
        // 列表不展示 committer；详情页会按需读取完整字段。
        committer: Signature {
            name: String::new(),
            email: String::new(),
            timestamp: author_timestamp,
        },
        subject,
        body: String::new(),
        refs,
    })
}

fn parse_log_output(text: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::new();
    for (index, record) in text.split('\x1e').enumerate() {
        let trimmed = record.trim_start_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        ensure_git_list_room(commits.len(), "Git 日志列表")?;
        ensure_git_message_size(trimmed.as_bytes(), "Git 日志记录", index + 1)?;
        let commit = parse_record(trimmed).map_err(|reason| {
            DomainError::QueryFailed(format!(
                "解析 Git 日志第 {} 条记录失败：{reason}",
                index + 1
            ))
        })?;
        commits.push(commit);
    }
    Ok(commits)
}

fn parse_record(record: &str) -> std::result::Result<Commit, String> {
    let mut fields = record.splitn(11, '\x1f');
    let hash = fields.next().ok_or("缺少 commit id")?.trim();
    if hash.is_empty() {
        return Err("commit id 为空".into());
    }
    let author_name = fields.next().ok_or("缺少作者名")?;
    let author_email = fields.next().ok_or("缺少作者邮箱")?;
    let author_ts = fields
        .next()
        .ok_or("缺少作者时间")?
        .parse::<i64>()
        .map_err(|error| format!("作者时间非整数：{error}"))?;
    let committer_name = fields.next().ok_or("缺少提交者名")?;
    let committer_email = fields.next().ok_or("缺少提交者邮箱")?;
    let committer_ts = fields
        .next()
        .ok_or("缺少提交时间")?
        .parse::<i64>()
        .map_err(|error| format!("提交时间非整数：{error}"))?;
    let parents_str = fields.next().ok_or("缺少父提交字段")?;
    // %D：decorate refs（"HEAD -> main, origin/main, tag: v1.0"），逗号分隔
    let refs = parse_refs(fields.next().ok_or("缺少引用字段")?);
    let subject = fields.next().ok_or("缺少提交主题")?.to_string();
    let body = fields
        .next()
        .ok_or("缺少提交正文字段")?
        .trim_end_matches('\n')
        .to_string();

    let author_timestamp = chrono::DateTime::from_timestamp(author_ts, 0)
        .ok_or_else(|| format!("作者时间超出支持范围：{author_ts}"))?;
    let committer_timestamp = chrono::DateTime::from_timestamp(committer_ts, 0)
        .ok_or_else(|| format!("提交时间超出支持范围：{committer_ts}"))?;

    let parents = parse_parents(parents_str)?;

    Ok(Commit {
        id: CommitId(hash.to_string()),
        parents,
        author: Signature {
            name: author_name.to_string(),
            email: author_email.to_string(),
            timestamp: author_timestamp,
        },
        committer: Signature {
            name: committer_name.to_string(),
            email: committer_email.to_string(),
            timestamp: committer_timestamp,
        },
        subject,
        body,
        refs,
    })
}

fn parse_parents(raw: &str) -> std::result::Result<Vec<CommitId>, String> {
    let mut parents = Vec::new();
    for parent in raw.split_whitespace().filter(|value| !value.is_empty()) {
        if parents.len() >= MAX_COMMIT_PARENTS {
            return Err(format!("父提交数量超过 {MAX_COMMIT_PARENTS} 个安全上限"));
        }
        parents.push(CommitId(parent.to_string()));
    }
    Ok(parents)
}

fn parse_refs(raw: &str) -> Vec<String> {
    let mut iter = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut refs: Vec<String> = iter
        .by_ref()
        .take(MAX_COMMIT_REFS)
        .map(str::to_string)
        .collect();
    let remaining = iter.count();
    if remaining > 0 {
        // 给提示腾一个位置，最终 Vec 始终不超过 MAX_COMMIT_REFS。
        refs.pop();
        refs.push(format!("…另有 {} 个引用已省略", remaining + 1));
    }
    refs
}

#[cfg(test)]
#[path = "log/tests.rs"]
mod tests;
