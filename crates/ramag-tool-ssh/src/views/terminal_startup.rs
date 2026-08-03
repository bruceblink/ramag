//! 拖入目录后等待远端 Shell 就绪并静默切换工作目录。

use std::time::{Duration, Instant};

use gpui::{Context, Window};
use ramag_domain::entities::{SshProfileId, validate_remote_path};
use ramag_terminal::TerminalSnapshot;

use super::SshView;
use super::model::Notice;

const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(80);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

enum StartupProbe {
    Waiting,
    Ready,
    Finished,
}

impl SshView {
    pub(super) fn enter_terminal_directory_when_ready(
        &self,
        workspace_id: SshProfileId,
        terminal_id: u64,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match directory_change_input(&path) {
            Ok(input) => input,
            Err(error) => {
                tracing::warn!(error = %error, "reject invalid terminal startup directory");
                return;
            }
        };
        cx.spawn_in(window, async move |this, async_cx| {
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            let mut prompt_seen = false;
            loop {
                async_cx
                    .background_executor()
                    .timer(STARTUP_POLL_INTERVAL)
                    .await;
                let Ok(probe) = this.update_in(async_cx, |this, _window, cx| {
                    let Some(terminal) = this
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.profile_id() == &workspace_id)
                        .and_then(|workspace| {
                            workspace
                                .terminals
                                .iter()
                                .find(|terminal| terminal.id == terminal_id)
                        })
                    else {
                        return StartupProbe::Finished;
                    };
                    let core = terminal.view.read(cx).core();
                    if core.exit_status().is_some() || core.is_closed() {
                        StartupProbe::Finished
                    } else if snapshot_has_shell_prompt(&core.snapshot()) {
                        StartupProbe::Ready
                    } else {
                        StartupProbe::Waiting
                    }
                }) else {
                    break;
                };
                match probe {
                    StartupProbe::Finished => break,
                    StartupProbe::Waiting => prompt_seen = false,
                    StartupProbe::Ready if !prompt_seen => prompt_seen = true,
                    StartupProbe::Ready => {
                        let send_result = this.update_in(async_cx, |this, _window, cx| {
                            this.workspaces
                                .iter()
                                .find(|workspace| workspace.profile_id() == &workspace_id)
                                .and_then(|workspace| {
                                    workspace
                                        .terminals
                                        .iter()
                                        .find(|terminal| terminal.id == terminal_id)
                                })
                                .ok_or_else(|| "目标终端已关闭".to_string())?
                                .view
                                .read(cx)
                                .core()
                                .send(input.clone())
                                .map_err(|error| error.to_string())
                        });
                        if let Ok(Err(error)) = send_result {
                            let _ = this.update_in(async_cx, |this, _window, cx| {
                                this.notice = Some(Notice::error(format!(
                                    "已连接，但自动进入目录失败：{error}"
                                )));
                                cx.notify();
                            });
                        }
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    tracing::warn!(
                        profile_id = %workspace_id,
                        terminal_id,
                        "terminal startup prompt detection timed out"
                    );
                    let _ = this.update_in(async_cx, |this, _window, cx| {
                        this.notice = Some(Notice::error("已连接，但未能自动进入拖入的目录"));
                        cx.notify();
                    });
                    break;
                }
            }
        })
        .detach();
    }
}

fn directory_change_input(path: &str) -> Result<Vec<u8>, String> {
    validate_remote_path(path)?;
    if !path.starts_with('/') {
        return Err("新终端的远程目录必须是绝对路径".into());
    }
    let escaped = path.replace('\'', "'\\''");
    Ok(format!("cd '{escaped}' && printf '\\033[2K\\r\\033[1A\\033[2K\\r'\r").into_bytes())
}

fn snapshot_has_shell_prompt(snapshot: &TerminalSnapshot) -> bool {
    let Some(cursor) = snapshot.cursor else {
        return false;
    };
    let Some(row) = snapshot.rows.get(cursor.row) else {
        return false;
    };
    let line = row
        .iter()
        .take(cursor.column.saturating_add(1))
        .map(|cell| cell.text.as_str())
        .collect::<String>();
    line_has_shell_prompt(&line)
}

fn line_has_shell_prompt(line: &str) -> bool {
    matches!(line.trim_end().chars().last(), Some('$' | '#' | '%' | '>'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_input_quotes_shell_text_and_erases_its_own_echo() {
        let input = directory_change_input("/srv/team's data/$(whoami)").unwrap();

        assert_eq!(
            String::from_utf8(input).unwrap(),
            "cd '/srv/team'\\''s data/$(whoami)' && printf '\\033[2K\\r\\033[1A\\033[2K\\r'\r"
        );
        assert!(directory_change_input("relative/path").is_err());
        assert!(directory_change_input("/tmp/line\nbreak").is_err());
    }

    #[test]
    fn prompt_detection_ignores_login_messages() {
        assert!(line_has_shell_prompt("[alice@server ~]$ "));
        assert!(line_has_shell_prompt("root@server:/srv# "));
        assert!(!line_has_shell_prompt("Connecting to server 1.2"));
        assert!(!line_has_shell_prompt(
            "Last login: Mon Aug 3 from 10.0.0.1"
        ));
        assert!(!line_has_shell_prompt("Password:"));
    }
}
