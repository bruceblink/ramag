//! 系统 OpenSSH 发现、能力探测与无 shell 参数构造。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use ramag_domain::entities::{SshAuthMode, SshCapability, SshLaunchCommand, SshProfile};
use ramag_domain::error::{DomainError, Result};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_READER_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_OUTPUT_LIMIT: usize = 16 * 1024;

#[derive(Clone, Default)]
pub struct OpenSshLocator {
    cache: Arc<Mutex<HashMap<PathBuf, SshCapability>>>,
}

impl OpenSshLocator {
    pub async fn probe(&self, custom_path: Option<String>) -> Result<SshCapability> {
        let candidates = discover_candidates(custom_path.as_deref())?;
        let explicit = custom_path.is_some();
        let mut last_error = None;

        for candidate in candidates {
            let path = match tokio::task::spawn_blocking(move || {
                validate_executable(&candidate, explicit)
            })
            .await
            .map_err(|error| {
                DomainError::Other(format!("校验 OpenSSH 路径任务异常退出：{error}"))
            })? {
                Ok(path) => path,
                Err(error) if explicit => return Err(error),
                Err(_) => continue,
            };
            if let Some(capability) = self.cache.lock().get(&path).cloned() {
                if path.is_file() {
                    return Ok(capability);
                }
                self.cache.lock().remove(&path);
            }
            match probe_version(&path).await {
                Ok(version) => {
                    let capability = SshCapability {
                        executable: path.to_string_lossy().into_owned(),
                        version,
                    };
                    self.cache.lock().insert(path, capability.clone());
                    return Ok(capability);
                }
                Err(error) if explicit => return Err(error),
                Err(error) => last_error = Some(error),
            }
        }

        if let Some(error) = last_error {
            return Err(error);
        }
        Err(DomainError::NotFound(missing_openssh_hint()))
    }

    pub fn invalidate(&self, executable: &str) {
        self.cache.lock().remove(Path::new(executable));
    }
}

pub fn terminal_command(
    profile: &SshProfile,
    capability: &SshCapability,
) -> Result<SshLaunchCommand> {
    profile.validate().map_err(DomainError::InvalidConfig)?;
    let mut args = vec!["-tt".to_string()];
    args.extend(common_profile_args(profile));
    args.push("--".into());
    args.push(profile.host.clone());
    Ok(SshLaunchCommand {
        profile_id: profile.id.clone(),
        program: capability.executable.clone(),
        args,
        env: HashMap::new(),
    })
}

pub fn sftp_args(profile: &SshProfile) -> Result<Vec<String>> {
    profile.validate().map_err(DomainError::InvalidConfig)?;
    let password_auth = profile.auth_mode == SshAuthMode::Password;
    let mut args = vec![
        "-T".into(),
        "-o".into(),
        format!("BatchMode={}", if password_auth { "no" } else { "yes" }),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        format!(
            "NumberOfPasswordPrompts={}",
            if password_auth { 1 } else { 0 }
        ),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    args.extend(common_profile_args(profile));
    args.push("-s".into());
    args.push("--".into());
    args.push(profile.host.clone());
    args.push("sftp".into());
    Ok(args)
}

fn common_profile_args(profile: &SshProfile) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(port) = profile.port {
        args.extend(["-p".into(), port.to_string()]);
    }
    if !profile.username.is_empty() {
        args.extend(["-l".into(), profile.username.clone()]);
    }
    match profile.auth_mode {
        SshAuthMode::Password => {
            args.extend([
                "-o".into(),
                "PreferredAuthentications=password,keyboard-interactive".into(),
                "-o".into(),
                "PubkeyAuthentication=no".into(),
            ]);
        }
        SshAuthMode::KeyFile => {
            if let Some(path) = profile.key_path.as_ref() {
                args.extend(["-i".into(), path.clone()]);
            }
        }
        SshAuthMode::System => {}
    }
    args
}

fn discover_candidates(custom_path: Option<&str>) -> Result<Vec<PathBuf>> {
    if let Some(value) = custom_path {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(DomainError::InvalidConfig(
                "自定义 OpenSSH 可执行文件必须是绝对路径".into(),
            ));
        }
        return Ok(vec![path]);
    }

    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    candidates.push(PathBuf::from("/usr/bin/ssh"));
    #[cfg(target_os = "windows")]
    if let Some(windir) = std::env::var_os("WINDIR") {
        let windir = PathBuf::from(windir);
        if windir.is_absolute() {
            candidates.push(windir.join("System32/OpenSSH/ssh.exe"));
        }
    }

    let current_directory = std::env::current_dir()
        .map_err(|error| DomainError::Other(format!("读取当前工作目录失败：{error}")))?;
    let current_directory = dunce::canonicalize(&current_directory)
        .map_err(|error| DomainError::Other(format!("解析当前工作目录失败：{error}")))?;
    let executable = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    if let Some(path_value) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path_value) {
            // 空路径和相对路径会隐式指向工作目录，必须排除。
            if directory.as_os_str().is_empty() || !directory.is_absolute() {
                continue;
            }
            // 绝对路径也可能显式指回工作目录；无法规范化的目录不可能安全启动。
            let Ok(directory) = dunce::canonicalize(directory) else {
                continue;
            };
            if paths_equal(&directory, &current_directory) {
                continue;
            }
            candidates.push(directory.join(executable));
        }
    }
    candidates.dedup();
    Ok(candidates)
}

fn validate_executable(path: &Path, explicit: bool) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(DomainError::InvalidConfig(
            "OpenSSH 可执行文件路径不是绝对路径".into(),
        ));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        let message = format!("OpenSSH 可执行文件不可访问（{}）：{error}", path.display());
        if explicit {
            DomainError::InvalidConfig(message)
        } else {
            DomainError::NotFound(message)
        }
    })?;
    if !metadata.is_file() {
        return Err(DomainError::InvalidConfig(format!(
            "OpenSSH 路径不是普通文件：{}",
            path.display()
        )));
    }
    dunce::canonicalize(path)
        .map_err(|error| DomainError::InvalidConfig(format!("解析 OpenSSH 绝对路径失败：{error}")))
}

async fn probe_version(path: &Path) -> Result<String> {
    let mut command = tokio::process::Command::new(path);
    command
        .arg("-V")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_no_window(command.as_std_mut());
    let mut child = command.spawn().map_err(|error| {
        DomainError::ConnectionFailed(format!("启动 OpenSSH 能力探测失败：{error}"))
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DomainError::Other("OpenSSH 能力探测未创建 stdout 管道".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| DomainError::Other("OpenSSH 能力探测未创建 stderr 管道".into()))?;
    let mut stdout_reader = tokio::spawn(read_bounded(stdout));
    let mut stderr_reader = tokio::spawn(read_bounded(stderr));
    let status = match timeout(PROBE_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            stop_probe(&mut child, &stdout_reader, &stderr_reader).await;
            return Err(DomainError::ConnectionFailed(format!(
                "等待 OpenSSH 能力探测失败：{error}"
            )));
        }
        Err(_) => {
            stop_probe(&mut child, &stdout_reader, &stderr_reader).await;
            return Err(DomainError::ConnectionFailed(
                "OpenSSH 能力探测在 3 秒内未完成".into(),
            ));
        }
    };
    let reader_result = timeout(PROBE_READER_TIMEOUT, async {
        let stdout = (&mut stdout_reader)
            .await
            .map_err(|error| format!("stdout 读取任务异常退出：{error}"))?
            .map_err(|error| format!("读取 stdout 失败：{error}"))?;
        let stderr = (&mut stderr_reader)
            .await
            .map_err(|error| format!("stderr 读取任务异常退出：{error}"))?
            .map_err(|error| format!("读取 stderr 失败：{error}"))?;
        Ok::<_, String>((stdout, stderr))
    })
    .await;
    let (stdout, stderr) = match reader_result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            stdout_reader.abort();
            stderr_reader.abort();
            return Err(DomainError::ConnectionFailed(format!(
                "读取 OpenSSH 能力探测结果失败：{error}"
            )));
        }
        Err(_) => {
            stdout_reader.abort();
            stderr_reader.abort();
            return Err(DomainError::ConnectionFailed(
                "读取 OpenSSH 能力探测结果超时".into(),
            ));
        }
    };
    let output = if stderr.is_empty() { stdout } else { stderr };
    let version = sanitized_output(&output);
    if !status.success() {
        return Err(DomainError::ConnectionFailed(format!(
            "OpenSSH 能力探测退出码为 {}{}",
            status,
            if version.is_empty() {
                String::new()
            } else {
                format!("：{version}")
            }
        )));
    }
    if version.is_empty() {
        return Err(DomainError::ConnectionFailed(
            "OpenSSH 能力探测未返回版本信息".into(),
        ));
    }
    if !version.to_ascii_lowercase().contains("openssh") {
        return Err(DomainError::ConnectionFailed(format!(
            "能力探测结果不是受支持的 OpenSSH：{version}"
        )));
    }
    Ok(version)
}

async fn read_bounded(mut reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(PROBE_OUTPUT_LIMIT.min(1024));
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await? {
            0 => break,
            read if output.len() < PROBE_OUTPUT_LIMIT => {
                let remaining = PROBE_OUTPUT_LIMIT - output.len();
                output.extend_from_slice(&chunk[..read.min(remaining)]);
            }
            _ => {}
        }
    }
    Ok(output)
}

async fn stop_probe(
    child: &mut tokio::process::Child,
    stdout_reader: &JoinHandle<io::Result<Vec<u8>>>,
    stderr_reader: &JoinHandle<io::Result<Vec<u8>>>,
) {
    if let Err(error) = child.start_kill() {
        tracing::warn!(error = %error, "kill openssh probe failed");
    }
    let _ = timeout(PROBE_READER_TIMEOUT, child.wait()).await;
    stdout_reader.abort();
    stderr_reader.abort();
}

fn sanitized_output(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>()
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(target_os = "windows"))]
    {
        left == right
    }
}

fn missing_openssh_hint() -> String {
    if cfg!(windows) {
        "未找到系统 OpenSSH Client。请在 Windows“可选功能”中安装 OpenSSH Client；也可主动以管理员身份运行 PowerShell 命令：Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0".into()
    } else {
        "未找到可用的系统 OpenSSH（已检查 /usr/bin/ssh 与 PATH 中的绝对目录）".into()
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn configure_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn configure_no_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> SshProfile {
        let mut profile = SshProfile::new("server", "example.com");
        profile.port = Some(2222);
        profile.username = "alice".into();
        profile
    }

    #[test]
    fn command_arguments_are_separate_and_end_options_before_host() {
        let mut profile = profile();
        profile.host = "server; touch /tmp/bad".into();
        assert!(
            terminal_command(
                &profile,
                &SshCapability {
                    executable: "/usr/bin/ssh".into(),
                    version: "OpenSSH_test".into(),
                }
            )
            .is_err()
        );

        profile.host = "server.example".into();
        let terminal = terminal_command(
            &profile,
            &SshCapability {
                executable: "/usr/bin/ssh".into(),
                version: "OpenSSH_test".into(),
            },
        )
        .unwrap();
        assert_eq!(
            &terminal.args[terminal.args.len() - 2..],
            ["--", "server.example"]
        );

        let sftp = sftp_args(&profile).unwrap();
        assert_eq!(&sftp[sftp.len() - 3..], ["--", "server.example", "sftp"]);
        assert!(sftp.windows(2).any(|args| args == ["-o", "BatchMode=yes"]));
        assert!(!sftp.iter().any(|arg| arg == "StrictHostKeyChecking=no"));
    }

    #[test]
    fn config_alias_keeps_config_port_and_hash_username_is_literal() {
        let mut profile = SshProfile::new("jump", "private-jump");
        profile.port = None;
        let args = sftp_args(&profile).unwrap();
        assert!(!args.iter().any(|arg| arg == "-p"));
        assert!(!args.iter().any(|arg| arg == "-l"));

        profile.username = "team#account#00000000-0000-0000-0000-000000000000".into();
        let args = sftp_args(&profile).unwrap();
        assert!(
            args.windows(2).any(|args| {
                args == ["-l", "team#account#00000000-0000-0000-0000-000000000000"]
            })
        );
    }

    #[test]
    fn password_mode_enables_one_askpass_attempt_without_exposing_secret() {
        let mut profile = profile();
        profile.auth_mode = SshAuthMode::Password;
        profile.password = "top-secret".into();

        let args = sftp_args(&profile).unwrap();
        assert!(args.windows(2).any(|args| args == ["-o", "BatchMode=no"]));
        assert!(
            args.windows(2)
                .any(|args| args == ["-o", "NumberOfPasswordPrompts=1"])
        );
        assert!(args.iter().all(|arg| !arg.contains("top-secret")));
    }

    #[test]
    fn custom_executable_must_be_absolute() {
        let error = discover_candidates(Some("ssh")).unwrap_err();
        assert!(matches!(error, DomainError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn output_is_bounded_and_sanitized() {
        let input = vec![b'a'; PROBE_OUTPUT_LIMIT + 100];
        assert_eq!(
            read_bounded(input.as_slice()).await.unwrap().len(),
            PROBE_OUTPUT_LIMIT
        );
        assert_eq!(
            sanitized_output(b"OpenSSH_test\0secret\nsecond"),
            "OpenSSH_testsecret"
        );
    }

    #[test]
    fn automatic_discovery_never_uses_current_directory() {
        let current = dunce::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let candidates = discover_candidates(None).unwrap();
        assert!(candidates.iter().all(|candidate| {
            candidate
                .parent()
                .is_none_or(|directory| !paths_equal(directory, &current))
        }));
    }
}
