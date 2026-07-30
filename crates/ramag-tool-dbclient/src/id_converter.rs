//! `@ID` 外部转换程序协议与资源边界。

use std::io;
use std::process::Stdio;
use std::time::Duration;

use smol::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};

const CONVERTER_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONVERTER_INPUT_BYTES: usize = ramag_ui::MAX_SEARCH_INPUT_BYTES;
const MAX_CONVERTER_STDOUT_BYTES: usize = 128;
const MAX_CONVERTER_STDERR_BYTES: usize = 4 * 1024;

pub(crate) async fn convert_id(program: &str, input: &str) -> Result<i64, String> {
    if input.is_empty() {
        return Err("ID 搜索词不能为空".to_string());
    }
    if input.len() > MAX_CONVERTER_INPUT_BYTES {
        return Err(format!(
            "ID 搜索词超过 {} KiB 上限",
            MAX_CONVERTER_INPUT_BYTES / 1024
        ));
    }
    if input.contains(['\0', '\r', '\n']) {
        return Err("ID 搜索词不能包含 NUL 或换行符".to_string());
    }

    let operation = run_converter(program, input);
    let timeout = async {
        smol::Timer::after(CONVERTER_TIMEOUT).await;
        Err("转换程序执行超过 2 秒，已终止".to_string())
    };
    smol::future::race(operation, timeout).await
}

async fn run_converter(program: &str, input: &str) -> Result<i64, String> {
    let mut command = std::process::Command::new(program);
    configure_no_window(&mut command);
    let mut command = smol::process::Command::from(command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|error| format!("启动转换程序失败：{error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "转换程序未创建 stdin 管道".to_string())?;
    stdin
        .write_all(input.as_bytes())
        .await
        .map_err(|error| format!("写入转换程序 stdin 失败：{error}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| format!("写入转换程序 stdin 失败：{error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("刷新转换程序 stdin 失败：{error}"))?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "转换程序未创建 stdout 管道".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "转换程序未创建 stderr 管道".to_string())?;
    let (status, stdout, stderr) = futures::try_join!(
        child.status(),
        read_bounded(stdout, MAX_CONVERTER_STDOUT_BYTES),
        read_bounded(stderr, MAX_CONVERTER_STDERR_BYTES),
    )
    .map_err(|error| format!("读取转换程序结果失败：{error}"))?;

    if !status.success() {
        let detail = sanitize_stderr(&stderr);
        return Err(if detail.is_empty() {
            format!("转换程序退出状态异常：{status}")
        } else {
            format!("转换程序失败（{status}）：{detail}")
        });
    }
    parse_converter_stdout(&stdout)
}

fn sanitize_stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

async fn read_bounded<R>(reader: R, limit: usize) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    reader
        .take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(io::Error::other(format!("输出超过 {limit} bytes 上限")));
    }
    Ok(bytes)
}

fn parse_converter_stdout(stdout: &[u8]) -> Result<i64, String> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| "转换程序 stdout 不是有效 UTF-8".to_string())?
        .trim();
    if text.is_empty() {
        return Err("转换程序 stdout 为空".to_string());
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("转换程序 stdout 必须只包含一个非负十进制整数".to_string());
    }
    let value = text
        .parse::<u64>()
        .map_err(|_| "转换程序输出的整数超出 u64 范围".to_string())?;
    i64::try_from(value).map_err(|_| "转换程序输出的整数超出 i64 范围".to_string())
}

#[cfg(windows)]
fn configure_no_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_no_window(_: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use super::{convert_id, parse_converter_stdout, sanitize_stderr};

    #[test]
    fn converter_stdout_accepts_one_decimal_integer() {
        assert_eq!(
            parse_converter_stdout(b"2062113056040685562\n").unwrap(),
            2_062_113_056_040_685_562
        );
        assert_eq!(parse_converter_stdout(b"0").unwrap(), 0);
    }

    #[test]
    fn converter_stdout_rejects_non_decimal_and_overflow() {
        assert!(parse_converter_stdout(b"12 extra").is_err());
        assert!(parse_converter_stdout(b"-1").is_err());
        assert!(parse_converter_stdout(b"9223372036854775808").is_err());
        assert!(parse_converter_stdout(&[0xff]).is_err());
    }

    #[test]
    fn converter_stderr_is_safe_for_inline_display() {
        assert_eq!(sanitize_stderr(b"bad\n\x1b[31mvalue\0"), "bad [31mvalue");
    }

    #[cfg(unix)]
    #[test]
    fn external_converter_receives_stdin_and_returns_decimal_id() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_SCRIPT: AtomicU64 = AtomicU64::new(0);
        let suffix = NEXT_SCRIPT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ramag-id-converter-{}-{suffix}.sh",
            std::process::id()
        ));
        let script = r#"#!/bin/sh
IFS= read -r value
if [ "$value" != "5MCjHfKUas3" ]; then
  echo "unexpected stdin" >&2
  exit 9
fi
printf '%s\n' '2062113056040685562'
"#;
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();

        let result = smol::block_on(convert_id(path.to_str().unwrap(), "5MCjHfKUas3"));
        let _ = std::fs::remove_file(path);

        assert_eq!(result.unwrap(), 2_062_113_056_040_685_562);
    }
}
