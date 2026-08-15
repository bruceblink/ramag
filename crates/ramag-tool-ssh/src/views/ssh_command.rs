//! OpenSSH 命令的无执行解析，用于连接表单自动填充。

use std::path::{Path, PathBuf};

use ramag_domain::entities::{
    MAX_SSH_HOST_BYTES, MAX_SSH_PATH_BYTES, MAX_SSH_USERNAME_BYTES, SshAuthMode, SshProfile,
};

pub(super) const MAX_SSH_COMMAND_BYTES: usize = 4096;

/// 生成可再次解析的 SSH 命令。密码不会写入命令文本，避免在明文输入框泄露。
pub(super) fn profile_ssh_command(profile: &SshProfile) -> String {
    let executable = profile.ssh_path.as_deref().unwrap_or("ssh");
    let mut arguments = vec![shell_argument(executable)];
    if !profile.username.is_empty() {
        arguments.push("-l".into());
        arguments.push(shell_argument(&profile.username));
    }
    if let Some(port) = profile.port {
        arguments.push("-p".into());
        arguments.push(port.to_string());
    }
    if profile.auth_mode == SshAuthMode::KeyFile
        && let Some(key_path) = profile.key_path.as_deref()
    {
        arguments.push("-i".into());
        arguments.push(shell_argument(key_path));
    }
    arguments.push(shell_argument(&profile.host));
    arguments.join(" ")
}

fn shell_argument(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedSshCommand {
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub key_path: Option<String>,
}

pub(super) fn parse_ssh_command(
    command: &str,
    user_home: Option<&Path>,
) -> Result<ParsedSshCommand, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("请粘贴 SSH 命令".into());
    }
    if command.len() > MAX_SSH_COMMAND_BYTES {
        return Err(format!("SSH 命令不能超过 {MAX_SSH_COMMAND_BYTES} 字节"));
    }
    if command
        .chars()
        .any(|character| character == '\0' || matches!(character, '\r' | '\n'))
    {
        return Err("SSH 命令必须是单行文本".into());
    }

    let arguments = tokenize(command)?;
    let Some(executable) = arguments.first() else {
        return Err("请粘贴 SSH 命令".into());
    };
    let executable_name = Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable);
    if !matches!(executable_name, "ssh" | "ssh.exe") {
        return Err("命令必须以 ssh 开头".into());
    }

    let mut username = None;
    let mut port = None;
    let mut key_path = None;
    let mut target = None;
    let mut index = 1usize;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            index = index.saturating_add(1);
            target = arguments.get(index).cloned();
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            target = Some(argument.clone());
            break;
        }

        if let Some(value) = compact_option(argument, "-p") {
            port = Some(parse_port(value)?);
        } else if argument == "-p" {
            index = index.saturating_add(1);
            port = Some(parse_port(option_argument(&arguments, index, "-p")?)?);
        } else if let Some(value) = compact_option(argument, "-l") {
            username = Some(value.to_string());
        } else if argument == "-l" {
            index = index.saturating_add(1);
            username = Some(option_argument(&arguments, index, "-l")?.to_string());
        } else if let Some(value) = compact_option(argument, "-i") {
            key_path = Some(expand_key_path(value, user_home)?);
        } else if argument == "-i" {
            index = index.saturating_add(1);
            key_path = Some(expand_key_path(
                option_argument(&arguments, index, "-i")?,
                user_home,
            )?);
        } else if let Some(value) = compact_option(argument, "-o") {
            apply_open_ssh_option(value, user_home, &mut username, &mut port, &mut key_path)?;
        } else if argument == "-o" {
            index = index.saturating_add(1);
            apply_open_ssh_option(
                option_argument(&arguments, index, "-o")?,
                user_home,
                &mut username,
                &mut port,
                &mut key_path,
            )?;
        } else if option_takes_value(argument) {
            index = index.saturating_add(1);
            let _ = option_argument(&arguments, index, argument)?;
        } else if !is_flag_option(argument) {
            return Err(format!("暂不支持 SSH 参数：{argument}"));
        }
        index = index.saturating_add(1);
    }

    let target = target.ok_or_else(|| "SSH 命令缺少目标主机".to_string())?;
    let (target_username, host) = split_target(&target)?;
    let username = target_username.or(username).unwrap_or_default();
    validate_field("主机", &host, MAX_SSH_HOST_BYTES)?;
    validate_field("用户名", &username, MAX_SSH_USERNAME_BYTES)?;
    if host.starts_with('-') || host.chars().any(char::is_whitespace) {
        return Err("SSH 目标主机格式无效".into());
    }

    Ok(ParsedSshCommand {
        host,
        port,
        username,
        key_path,
    })
}

fn tokenize(command: &str) -> Result<Vec<String>, String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut has_content = false;

    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            has_content = true;
            continue;
        }
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else {
                    current.push(character);
                }
                has_content = true;
            }
            Some('"') => {
                if character == '"' {
                    quote = None;
                } else if character == '\\' {
                    escaped = true;
                } else {
                    current.push(character);
                }
                has_content = true;
            }
            Some(_) => unreachable!(),
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    has_content = true;
                }
                '\\' => {
                    escaped = true;
                    has_content = true;
                }
                character if character.is_whitespace() => {
                    if has_content {
                        arguments.push(std::mem::take(&mut current));
                        has_content = false;
                    }
                }
                _ => {
                    current.push(character);
                    has_content = true;
                }
            },
        }
    }
    if escaped || quote.is_some() {
        return Err("SSH 命令包含未闭合的引号或转义符".into());
    }
    if has_content {
        arguments.push(current);
    }
    Ok(arguments)
}

fn compact_option<'a>(argument: &'a str, option: &str) -> Option<&'a str> {
    argument
        .strip_prefix(option)
        .filter(|value| !value.is_empty())
}

fn option_argument<'a>(
    arguments: &'a [String],
    index: usize,
    option: &str,
) -> Result<&'a str, String> {
    arguments
        .get(index)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("SSH 参数 {option} 缺少值"))
}

fn option_takes_value(argument: &str) -> bool {
    matches!(
        argument,
        "-b" | "-c"
            | "-D"
            | "-E"
            | "-e"
            | "-F"
            | "-I"
            | "-J"
            | "-L"
            | "-m"
            | "-O"
            | "-Q"
            | "-R"
            | "-S"
            | "-W"
            | "-w"
    )
}

fn is_flag_option(argument: &str) -> bool {
    argument.len() >= 2
        && argument[1..].chars().all(|flag| {
            matches!(
                flag,
                '4' | '6'
                    | 'A'
                    | 'a'
                    | 'C'
                    | 'f'
                    | 'G'
                    | 'g'
                    | 'K'
                    | 'k'
                    | 'M'
                    | 'N'
                    | 'n'
                    | 'q'
                    | 's'
                    | 'T'
                    | 't'
                    | 'V'
                    | 'v'
                    | 'X'
                    | 'x'
                    | 'Y'
                    | 'y'
            )
        })
}

fn apply_open_ssh_option(
    option: &str,
    user_home: Option<&Path>,
    username: &mut Option<String>,
    port: &mut Option<u16>,
    key_path: &mut Option<String>,
) -> Result<(), String> {
    let Some((name, value)) = option.split_once('=') else {
        return Ok(());
    };
    if name.eq_ignore_ascii_case("user") {
        *username = Some(value.to_string());
    } else if name.eq_ignore_ascii_case("port") {
        *port = Some(parse_port(value)?);
    } else if name.eq_ignore_ascii_case("identityfile") {
        *key_path = Some(expand_key_path(value, user_home)?);
    }
    Ok(())
}

fn split_target(target: &str) -> Result<(Option<String>, String), String> {
    if target.is_empty() {
        return Err("SSH 命令缺少目标主机".into());
    }
    if let Some((username, host)) = target.rsplit_once('@') {
        if username.is_empty() || host.is_empty() {
            return Err("SSH 目标必须使用 user@host 格式".into());
        }
        return Ok((Some(username.to_string()), host.to_string()));
    }
    Ok((None, target.to_string()))
}

fn parse_port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| "SSH 端口必须是 1 - 65535".to_string())
}

fn expand_key_path(value: &str, user_home: Option<&Path>) -> Result<String, String> {
    let expanded = if let Some(suffix) = value.strip_prefix("~/") {
        user_home
            .map(|path| path.join(suffix))
            .ok_or_else(|| "无法解析密钥路径中的 ~，请改用绝对路径".to_string())?
    } else {
        PathBuf::from(value)
    };
    if !expanded.is_absolute() {
        return Err("SSH 密钥路径必须是绝对路径".into());
    }
    let path = expanded
        .to_str()
        .ok_or_else(|| "SSH 密钥路径不是 UTF-8".to_string())?;
    validate_field("密钥路径", path, MAX_SSH_PATH_BYTES)?;
    Ok(path.to_string())
}

fn validate_field(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!("{label}不能超过 {max_bytes} 字节"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn absolute_test_key_path(name: &str) -> String {
        std::env::temp_dir()
            .join(name)
            .to_str()
            .unwrap()
            .replace(std::path::MAIN_SEPARATOR, "/")
    }

    #[test]
    fn parses_common_ssh_command() {
        let user_home = std::env::temp_dir().join("ramag-ssh-command-user");
        let expected_key_path = user_home.join(".ssh/id_ed25519");
        let parsed = parse_ssh_command(
            "ssh -p 2222 -i ~/.ssh/id_ed25519 alice@example.com",
            Some(&user_home),
        )
        .unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.username, "alice");
        assert_eq!(parsed.port, Some(2222));
        assert_eq!(parsed.key_path.as_deref(), expected_key_path.to_str());
    }

    #[test]
    fn parses_quoted_options_and_open_ssh_o_values() {
        let key_path = absolute_test_key_path("ramag-ssh-command/team key");
        let command = format!(r#"ssh -o User=deploy -o Port=2200 -i "{key_path}" server"#);
        let parsed = parse_ssh_command(&command, None).unwrap();
        assert_eq!(parsed.username, "deploy");
        assert_eq!(parsed.port, Some(2200));
        assert_eq!(parsed.key_path.as_deref(), Some(key_path.as_str()));
    }

    #[test]
    fn target_username_overrides_l_option() {
        let parsed = parse_ssh_command("ssh -l old new@host", None).unwrap();
        assert_eq!(parsed.username, "new");
    }

    #[test]
    fn rejects_non_ssh_or_incomplete_commands() {
        assert!(parse_ssh_command("scp file host:/tmp", None).is_err());
        assert!(parse_ssh_command("ssh -p 22", None).is_err());
        assert!(parse_ssh_command("ssh -p invalid host", None).is_err());
        assert!(parse_ssh_command("ssh 'unterminated", None).is_err());
    }

    #[test]
    fn edit_command_roundtrips_profile_without_exposing_password() {
        let mut profile = SshProfile::new("prod", "jump.example.com");
        profile.username = "deploy".into();
        profile.port = Some(2222);
        profile.auth_mode = SshAuthMode::Password;
        profile.password = "visible-secret".into();

        let command = profile_ssh_command(&profile);
        assert!(!command.contains("visible-secret"));
        let parsed = parse_ssh_command(&command, None).unwrap();
        assert_eq!(parsed.host, profile.host);
        assert_eq!(parsed.username, profile.username);
        assert_eq!(parsed.port, profile.port);
    }

    #[test]
    fn edit_command_quotes_key_paths_with_spaces() {
        let mut profile = SshProfile::new("key", "server");
        profile.auth_mode = SshAuthMode::KeyFile;
        profile.key_path = Some(absolute_test_key_path("ramag-ssh-command/team key"));

        let parsed = parse_ssh_command(&profile_ssh_command(&profile), None).unwrap();
        assert_eq!(parsed.key_path, profile.key_path);
    }
}
