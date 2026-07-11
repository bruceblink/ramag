//! Interactive rebase。plan：log onto..HEAD 全标 Pick；execute：用临时脚本作 GIT_SEQUENCE_EDITOR 注入 todo

use std::path::{Path, PathBuf};

use ramag_domain::entities::{RebaseAction, RebaseTodo};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{command, run_git_text};

/// onto..HEAD 最老在前，全部 Pick
pub fn plan(repo_path: &Path, onto: &str) -> Result<Vec<RebaseTodo>> {
    let out = run_git_text(
        repo_path,
        &[
            "log",
            "--format=%H %s",
            "--reverse",
            &format!("{onto}..HEAD"),
        ],
    )?;
    let mut todos = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((hash, subject)) = line.split_once(' ') {
            todos.push(RebaseTodo {
                action: RebaseAction::Pick,
                hash: hash.to_string(),
                subject: subject.to_string(),
            });
        }
    }
    Ok(todos)
}

/// 临时 shell 脚本作 GIT_SEQUENCE_EDITOR，避免弹出 $EDITOR。
/// Git for Windows 自带 POSIX shell，Windows 上显式通过 `sh` 执行，无需可执行权限位。
pub fn execute(repo_path: &Path, onto: &str, todos: &[RebaseTodo]) -> Result<()> {
    let todo_content: String = todos
        .iter()
        .map(|t| format!("{} {} {}\n", t.action.as_str(), t.short_hash(), t.subject))
        .collect();

    let tag = nano_id();
    let tmp_todo = std::env::temp_dir().join(format!("ramag_rebase_{tag}.txt"));
    let tmp_script = std::env::temp_dir().join(format!("ramag_seq_editor_{tag}.sh"));
    let _cleanup = TempFiles([tmp_todo.clone(), tmp_script.clone()]);
    std::fs::write(&tmp_todo, &todo_content)
        .map_err(|e| DomainError::Other(format!("写 rebase todo 失败: {e}")))?;

    let script = "#!/bin/sh\nset -eu\ncp \"$RAMAG_REBASE_TODO\" \"$1\"\n";
    std::fs::write(&tmp_script, script)
        .map_err(|e| DomainError::Other(format!("写 sequence editor 脚本失败: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&tmp_script)
            .map_err(|e| DomainError::Other(format!("读取 sequence editor 权限失败: {e}")))?;
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp_script, perms)
            .map_err(|e| DomainError::Other(format!("设置 sequence editor 权限失败: {e}")))?;
    }

    let script_str = shell_path(&tmp_script)?;
    let todo_path_str = shell_path(&tmp_todo)?;
    let sequence_editor = sequence_editor_command(&script_str);

    let output = command()
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
        .current_dir(repo_path)
        .output()
        .map_err(|e| DomainError::Other(format!("执行 git rebase -i 失败: {e}")))?;

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

struct TempFiles([PathBuf; 2]);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn nano_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}_{ns:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::{sequence_editor_command, shell_quote};

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
}
