//! 拖入目录后等待远端 Shell 就绪并静默切换工作目录。

use std::time::{Duration, Instant};

use gpui::{Context, Window};
use ramag_domain::entities::{RemotePath, RemoteShellKind, SshProfileId, validate_remote_path};
use ramag_terminal::TerminalSnapshot;

use super::SshView;
use super::model::Notice;

const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(80);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_WINDOWS_LOGIN_HISTORY_LINES: usize = 8;

enum StartupProbe {
    Waiting,
    Ready,
    Finished,
}

enum WindowsDirectoryProbe {
    Waiting,
    Load(String),
    Finished,
}

impl SshView {
    pub(super) fn enter_terminal_directory_when_ready(
        &self,
        workspace_id: SshProfileId,
        terminal_id: u64,
        path: String,
        shell: RemoteShellKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match directory_change_input(&path, shell) {
            Ok(input) => input,
            Err(error) => {
                tracing::warn!(
                    operation = "ssh_terminal_startup_directory",
                    profile_id = %workspace_id,
                    terminal_id,
                    path_bytes = path.len(),
                    error = %error,
                    "reject invalid terminal startup directory"
                );
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
                        operation = "ssh_terminal_startup_directory",
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

    /// Windows JumpServer 可能不支持无 PTY 探测，但交互终端提示符仍是可信平台证据。
    pub(super) fn load_windows_directory_from_terminal_when_ready(
        &self,
        workspace_id: SshProfileId,
        terminal_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, async_cx| {
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            let mut detected_directory = None;
            loop {
                async_cx
                    .background_executor()
                    .timer(STARTUP_POLL_INTERVAL)
                    .await;
                let Ok(probe) = this.update_in(async_cx, |this, _window, cx| {
                    let Some(workspace) = this
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.profile_id() == &workspace_id)
                    else {
                        return WindowsDirectoryProbe::Finished;
                    };
                    let Some(terminal) = workspace
                        .terminals
                        .iter()
                        .find(|terminal| terminal.id == terminal_id)
                    else {
                        return WindowsDirectoryProbe::Finished;
                    };
                    if detected_directory.is_none() {
                        let terminal_view = terminal.view.read(cx);
                        let core = terminal_view.core();
                        detected_directory = windows_directory_from_snapshot(&core.snapshot());
                        if detected_directory.is_some() {
                            core.reveal_short_initial_history(MAX_WINDOWS_LOGIN_HISTORY_LINES);
                        }
                    }
                    if !workspace.entries.is_empty()
                        || !matches!(workspace.path.as_str(), "." | "/")
                    {
                        return WindowsDirectoryProbe::Finished;
                    }
                    if should_load_terminal_directory_fallback(
                        workspace.directory_loaded,
                        workspace.sftp_loading,
                    ) && let Some(directory) = detected_directory.clone()
                    {
                        WindowsDirectoryProbe::Load(directory)
                    } else {
                        WindowsDirectoryProbe::Waiting
                    }
                }) else {
                    break;
                };
                match probe {
                    WindowsDirectoryProbe::Waiting => {}
                    WindowsDirectoryProbe::Finished => break,
                    WindowsDirectoryProbe::Load(directory) => {
                        // 先用 CMD 提示符修正 Auto 平台，再从虚拟根重建 Windows 文件通道。
                        // 必须请求“/”，用户目录由盘符列表继续导航。
                        let workspace_id = workspace_id.clone();
                        let _ = this.update_in(async_cx, move |this, _window, cx| {
                            let Some(profile) = this
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.profile_id() == &workspace_id)
                                .map(|workspace| workspace.profile.clone())
                            else {
                                return;
                            };
                            match this
                                .service
                                .remember_terminal_windows(&profile, RemoteShellKind::Cmd)
                            {
                                Ok(capabilities) => {
                                    if let Some(workspace) = this.workspace_mut(&workspace_id) {
                                        workspace.capabilities = Some(capabilities);
                                        workspace.sftp_error = None;
                                    }
                                    tracing::info!(
                                        operation = "ssh_terminal_platform_detect",
                                        profile_id = %workspace_id,
                                        terminal_id,
                                        terminal_directory = %directory,
                                        "windows platform inferred from interactive terminal"
                                    );
                                    this.bootstrap_directory_at(workspace_id, "/".into(), cx);
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        operation = "ssh_terminal_platform_detect",
                                        profile_id = %workspace_id,
                                        terminal_id,
                                        error = %error,
                                        "remember windows terminal evidence failed"
                                    );
                                    this.notice = Some(Notice::error(error.to_string()));
                                    cx.notify();
                                }
                            }
                        });
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
        })
        .detach();
    }
}

fn should_load_terminal_directory_fallback(directory_loaded: bool, sftp_loading: bool) -> bool {
    directory_loaded && !sftp_loading
}

fn directory_change_input(path: &str, shell: RemoteShellKind) -> Result<Vec<u8>, String> {
    validate_remote_path(path)?;
    RemotePath::parse_server_canonical(path)?;
    let shell = inferred_shell_for_directory(path, shell);
    match shell {
        RemoteShellKind::Posix => {
            if !path.starts_with('/') {
                return Err("POSIX 终端目录必须以 / 开头".into());
            }
            let escaped = path.replace('\'', "'\\''");
            Ok(format!("cd '{escaped}' && printf '\\033[2K\\r\\033[1A\\033[2K\\r'\r").into_bytes())
        }
        RemoteShellKind::Cmd => {
            let path = windows_shell_path(path)?;
            if path.contains(['"', '%', '!']) {
                return Err("Windows CMD 自动进入目录不支持双引号、% 或 !".into());
            }
            Ok(format!("cd /d \"{path}\"\r").into_bytes())
        }
        RemoteShellKind::WindowsPowerShell | RemoteShellKind::PowerShellCore => {
            let path = windows_shell_path(path)?;
            let escaped = path.replace('\'', "''");
            Ok(format!("Set-Location -LiteralPath '{escaped}'\r").into_bytes())
        }
        RemoteShellKind::Unknown => Err("未识别远端 Shell，不能安全地自动进入目录".into()),
    }
}

fn inferred_shell_for_directory(path: &str, shell: RemoteShellKind) -> RemoteShellKind {
    if shell != RemoteShellKind::Unknown {
        return shell;
    }
    if windows_shell_path(path).is_ok() {
        RemoteShellKind::Cmd
    } else if path.starts_with('/') {
        RemoteShellKind::Posix
    } else {
        RemoteShellKind::Unknown
    }
}

fn windows_shell_path(path: &str) -> Result<&str, String> {
    let path = path
        .strip_prefix('/')
        .filter(|value| is_drive_path(value))
        .unwrap_or(path);
    if !is_drive_path(path) {
        return Err("Windows 终端只能自动进入可映射的盘符目录".into());
    }
    Ok(path)
}

fn is_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn snapshot_has_shell_prompt(snapshot: &TerminalSnapshot) -> bool {
    snapshot_prompt_line(snapshot).is_some_and(|line| line_has_shell_prompt(&line))
}

fn snapshot_prompt_line(snapshot: &TerminalSnapshot) -> Option<String> {
    let cursor = snapshot.cursor?;
    let row = snapshot.rows.get(cursor.row)?;
    Some(
        row.iter()
            .take(cursor.column.saturating_add(1))
            .map(|cell| cell.text.as_str())
            .collect::<String>(),
    )
}

fn line_has_shell_prompt(line: &str) -> bool {
    matches!(line.trim_end().chars().last(), Some('$' | '#' | '%' | '>'))
}

fn windows_directory_from_snapshot(snapshot: &TerminalSnapshot) -> Option<String> {
    windows_directory_from_prompt(&snapshot_prompt_line(snapshot)?)
}

fn windows_directory_from_prompt(line: &str) -> Option<String> {
    let line = line.trim_end();
    let prompt = line.strip_suffix('>')?.trim_end();
    let bytes = prompt.as_bytes();
    for index in 0..bytes.len().saturating_sub(2) {
        if bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && matches!(bytes[index + 2], b'\\' | b'/')
            && (index == 0 || bytes[index - 1].is_ascii_whitespace())
        {
            let directory = prompt[index..].replace('\\', "/");
            if RemotePath::parse_server_canonical(&directory).is_ok() {
                return Some(directory);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_input_quotes_shell_text_and_erases_its_own_echo() {
        let input =
            directory_change_input("/srv/team's data/$(whoami)", RemoteShellKind::Posix).unwrap();

        assert_eq!(
            String::from_utf8(input).unwrap(),
            "cd '/srv/team'\\''s data/$(whoami)' && printf '\\033[2K\\r\\033[1A\\033[2K\\r'\r"
        );
        assert!(directory_change_input("relative/path", RemoteShellKind::Posix).is_err());
        assert!(directory_change_input("/tmp/line\nbreak", RemoteShellKind::Posix).is_err());
    }

    #[test]
    fn directory_input_uses_the_detected_windows_shell() {
        assert_eq!(
            String::from_utf8(
                directory_change_input("C:/Users/Administrator", RemoteShellKind::Cmd).unwrap()
            )
            .unwrap(),
            "cd /d \"C:/Users/Administrator\"\r"
        );
        assert_eq!(
            String::from_utf8(
                directory_change_input(
                    "/C:/Program Files/App",
                    RemoteShellKind::WindowsPowerShell,
                )
                .unwrap()
            )
            .unwrap(),
            "Set-Location -LiteralPath 'C:/Program Files/App'\r"
        );
        assert!(
            directory_change_input("/virtual/root", RemoteShellKind::Cmd).is_err(),
            "SFTP 虚拟路径不能猜测成 Windows 文件系统路径"
        );
    }

    #[test]
    fn unknown_shell_is_inferred_from_an_absolute_dragged_directory() {
        assert_eq!(
            String::from_utf8(
                directory_change_input("/opt/app", RemoteShellKind::Unknown).unwrap()
            )
            .unwrap(),
            "cd '/opt/app' && printf '\\033[2K\\r\\033[1A\\033[2K\\r'\r"
        );
        assert_eq!(
            String::from_utf8(
                directory_change_input("C:/Users/Administrator", RemoteShellKind::Unknown).unwrap()
            )
            .unwrap(),
            "cd /d \"C:/Users/Administrator\"\r"
        );
    }

    #[test]
    fn windows_prompt_exposes_the_real_terminal_directory() {
        assert_eq!(
            windows_directory_from_prompt("administrator@CAE365BE C:\\Users\\Administrator>"),
            Some("C:/Users/Administrator".into())
        );
        assert_eq!(
            windows_directory_from_prompt("PS D:\\部署目录\\服务>"),
            Some("D:/部署目录/服务".into())
        );
        assert_eq!(windows_directory_from_prompt("[root@host ~]#"), None);
    }

    #[test]
    fn windows_terminal_directory_fallback_waits_for_drive_discovery() {
        assert!(!should_load_terminal_directory_fallback(false, true));
        assert!(!should_load_terminal_directory_fallback(true, true));
        assert!(should_load_terminal_directory_fallback(true, false));
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
