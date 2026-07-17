//! Interactive rebase。plan：log onto..HEAD 全标 Pick；execute：用临时脚本作 GIT_SEQUENCE_EDITOR 注入 todo

use std::fmt::Write as _;
use std::path::Path;

use ramag_domain::entities::{RebaseAction, RebaseTodo};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    MAX_GIT_RECORD_BYTES, command, ensure_git_record_size, run_command_output_limited, run_git_text,
};
use crate::temp_file::TempFile;

const MAX_REBASE_TODOS: usize = 10_000;
const MAX_REBASE_TODO_BYTES: usize = 4 * 1024 * 1024;

/// onto..HEAD 最老在前，全部 Pick
pub fn plan(repo_path: &Path, onto: &str) -> Result<Vec<RebaseTodo>> {
    validate_revision_arg(onto)?;
    let out = run_git_text(
        repo_path,
        &[
            "log",
            "--format=%H %s",
            "--reverse",
            &format!("{onto}..HEAD"),
        ],
    )?;
    parse_plan_output(&out)
}

fn parse_plan_output(out: &str) -> Result<Vec<RebaseTodo>> {
    let mut todos = Vec::new();
    let mut retained_bytes = 0usize;
    for (line_index, line) in out.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if todos.len() >= MAX_REBASE_TODOS {
            return Err(DomainError::QueryFailed(format!(
                "rebase 计划超过 {MAX_REBASE_TODOS} 个 commit 安全上限，请缩小上游范围"
            )));
        }
        ensure_git_record_size(line.as_bytes(), "Git rebase 计划记录", line_index + 1)?;
        retained_bytes = retained_bytes.saturating_add(line.len());
        if retained_bytes > MAX_REBASE_TODO_BYTES {
            return Err(DomainError::QueryFailed(format!(
                "rebase 计划文本超过 {} MiB 安全上限，请缩小上游范围",
                MAX_REBASE_TODO_BYTES / 1024 / 1024
            )));
        }
        // format 在 hash 后固定放一个空格；空 subject 会只剩 hash，仍须保留该 commit。
        let (hash, subject) = line.split_once(' ').unwrap_or((line, ""));
        if !is_safe_object_id(hash) {
            return Err(DomainError::QueryFailed(format!(
                "解析 rebase 计划第 {} 条记录失败：commit id 无效",
                line_index + 1
            )));
        }
        todos.push(RebaseTodo {
            action: RebaseAction::Pick,
            hash: hash.to_string(),
            subject: subject.to_string(),
        });
    }
    Ok(todos)
}

/// 临时 shell 脚本作 GIT_SEQUENCE_EDITOR，避免弹出 $EDITOR。
/// Git for Windows 自带 POSIX shell，Windows 上显式通过 `sh` 执行，无需可执行权限位。
pub fn execute(repo_path: &Path, onto: &str, todos: &[RebaseTodo]) -> Result<()> {
    validate_revision_arg(onto)?;
    let todo_content = build_todo_content(todos)?;

    let tmp_todo = TempFile::create("ramag_rebase", "txt", todo_content.as_bytes())?;

    let script = "#!/bin/sh\nset -eu\ncp \"$RAMAG_REBASE_TODO\" \"$1\"\n";
    let tmp_script = TempFile::create("ramag_seq_editor", "sh", script.as_bytes())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(tmp_script.path())
            .map_err(|e| DomainError::Other(format!("读取 sequence editor 权限失败: {e}")))?;
        let mut perms = meta.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(tmp_script.path(), perms)
            .map_err(|e| DomainError::Other(format!("设置 sequence editor 权限失败: {e}")))?;
    }

    let script_str = shell_path(tmp_script.path())?;
    let todo_path_str = shell_path(tmp_todo.path())?;
    let sequence_editor = sequence_editor_command(&script_str);

    let mut rebase = command();
    rebase
        .args([
            "-c",
            "core.quotepath=false",
            "-c",
            "core.longpaths=true",
            "rebase",
            "-i",
            onto,
        ])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("GIT_SEQUENCE_EDITOR", sequence_editor)
        .env("GIT_EDITOR", "true")
        .env("RAMAG_REBASE_TODO", todo_path_str)
        .current_dir(repo_path);
    let output = run_command_output_limited(rebase, "rebase -i")?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    // 大写 CONFLICT 是 git 冲突标记，进入冲突解决态算正常推进；不匹配小写以免吞掉真实失败
    if output.status.success() || stderr.contains("CONFLICT") {
        Ok(())
    } else {
        Err(DomainError::QueryFailed(crate::errors::friendly_git_error(
            &["rebase", "-i", onto],
            &stderr,
        )))
    }
}

fn build_todo_content(todos: &[RebaseTodo]) -> Result<String> {
    let capacity = validate_todos(todos)?;

    let mut content = String::with_capacity(capacity);
    for todo in todos {
        writeln!(
            content,
            "{} {} {}",
            todo.action.as_str(),
            todo.short_hash(),
            sanitize_todo_subject(&todo.subject)
        )
        .map_err(|error| DomainError::Other(format!("生成 rebase 计划失败：{error}")))?;
    }
    Ok(content)
}

pub(crate) fn validate_todos(todos: &[RebaseTodo]) -> Result<usize> {
    if todos.is_empty() {
        return Err(DomainError::InvalidConfig("rebase 计划不能为空".into()));
    }
    if todos.len() > MAX_REBASE_TODOS {
        return Err(DomainError::InvalidConfig(format!(
            "rebase 计划超过 {MAX_REBASE_TODOS} 个 commit 安全上限"
        )));
    }
    let mut capacity = 0usize;
    for todo in todos {
        if !is_safe_object_id(&todo.hash) {
            return Err(DomainError::InvalidConfig(format!(
                "rebase commit id 无效：{}",
                todo.hash
            )));
        }
        if todo.subject.len() > MAX_GIT_RECORD_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "rebase commit {} 的标题超过 {} KiB 上限",
                todo.short_hash(),
                MAX_GIT_RECORD_BYTES / 1024
            )));
        }
        capacity = capacity
            .checked_add(
                todo.action
                    .as_str()
                    .len()
                    .saturating_add(todo.short_hash().len())
                    .saturating_add(todo.subject.len())
                    .saturating_add(3),
            )
            .ok_or_else(|| DomainError::InvalidConfig("rebase 计划大小溢出".into()))?;
        if capacity > MAX_REBASE_TODO_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "rebase 计划文本超过 {} MiB 安全上限",
                MAX_REBASE_TODO_BYTES / 1024 / 1024
            )));
        }
    }

    Ok(capacity)
}

/// Git 把 editor 当 shell 命令解析，因此路径必须可靠引用；Windows 需显式交给自带的 sh。
fn sequence_editor_command(script_path: &str) -> String {
    let quoted = shell_quote(script_path);
    if cfg!(target_os = "windows") {
        format!("sh {quoted}")
    } else {
        quoted
    }
}

fn shell_path(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| DomainError::Other("临时路径含非 UTF-8".into()))?;
    if cfg!(target_os = "windows") {
        Ok(value.replace('\\', "/"))
    } else {
        Ok(value.to_string())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn validate_revision_arg(revision: &str) -> Result<()> {
    crate::git_cmd::validate_positional_arg(revision, "rebase 上游引用")
}

fn is_safe_object_id(value: &str) -> bool {
    (4..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sanitize_todo_subject(subject: &str) -> String {
    subject
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_REBASE_TODOS, build_todo_content, parse_plan_output, sanitize_todo_subject,
        sequence_editor_command, shell_quote, validate_revision_arg,
    };
    use ramag_domain::entities::{RebaseAction, RebaseTodo};

    #[test]
    fn shell_quote_handles_spaces_and_apostrophes() {
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_quote("/tmp/a'b"), "'/tmp/a'\"'\"'b'");
    }

    #[test]
    fn sequence_editor_is_an_explicit_shell_command_on_windows() {
        let command = sequence_editor_command("C:\\Temp\\ramag editor.sh");
        if cfg!(target_os = "windows") {
            assert!(command.starts_with("sh "));
        } else {
            assert!(!command.starts_with("sh "));
        }
        assert!(command.contains("ramag editor.sh"));
    }

    #[test]
    fn plan_keeps_commit_with_empty_subject() -> ramag_domain::error::Result<()> {
        let hash = "a".repeat(40);
        let plan = parse_plan_output(&format!("{hash} \n"))?;
        assert_eq!(plan.len(), 1);
        assert!(plan[0].subject.is_empty());
        Ok(())
    }

    #[test]
    fn rebase_plan_budget_rejects_excessive_items() {
        let todo = RebaseTodo {
            action: RebaseAction::Pick,
            hash: "a".repeat(40),
            subject: "subject".into(),
        };
        assert!(build_todo_content(&vec![todo; MAX_REBASE_TODOS + 1]).is_err());
    }

    #[test]
    fn rebase_arguments_and_subject_are_sanitized() {
        assert!(validate_revision_arg("main").is_ok());
        assert!(validate_revision_arg("--exec=bad").is_err());
        assert_eq!(
            sanitize_todo_subject("first\nexec bad\r"),
            "first exec bad "
        );
        assert!(parse_plan_output("not-a-hash subject\n").is_err());
    }
}
