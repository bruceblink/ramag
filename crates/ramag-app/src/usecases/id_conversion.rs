//! ID 转换用例：进程内通用进制与外部转换器进程边界。

use std::io;
use std::process::Stdio;
use std::time::Duration;

use ramag_domain::entities::{IdConverterConfig, IdConverterKind, parse_nonnegative_id_integer};
use smol::io::{AsyncRead, AsyncReadExt as _};

const CONVERTER_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONVERTER_INPUT_BYTES: usize = 4 * 1024;
const MAX_CONVERTER_INTEGER_STDOUT_BYTES: usize = 128;
const MAX_CONVERTER_STRING_STDOUT_BYTES: usize = 4 * 1024;
const MAX_CONVERTER_STDERR_BYTES: usize = 4 * 1024;

pub async fn convert_id_to_integer(config: &IdConverterConfig, input: &str) -> Result<i64, String> {
    validate_input(input)?;
    config.validate_active()?;
    if !matches!(config.kind, IdConverterKind::ExternalProgram) {
        return config.decode_local(input);
    }

    let stdout = run_external_converter_with_timeout(
        &config.external_program,
        "-s",
        input,
        MAX_CONVERTER_INTEGER_STDOUT_BYTES,
    )
    .await?;
    parse_integer_stdout(&stdout)
}

pub async fn convert_id_to_string(
    config: &IdConverterConfig,
    input: &str,
) -> Result<String, String> {
    validate_input(input)?;
    config.validate_active()?;
    let value = parse_nonnegative_id_integer(input)?;
    if !matches!(config.kind, IdConverterKind::ExternalProgram) {
        return config.encode_local(value);
    }

    let canonical_input = value.to_string();
    let stdout = run_external_converter_with_timeout(
        &config.external_program,
        "-i",
        &canonical_input,
        MAX_CONVERTER_STRING_STDOUT_BYTES,
    )
    .await?;
    parse_string_stdout(&stdout)
}

fn validate_input(input: &str) -> Result<(), String> {
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
    Ok(())
}

async fn run_external_converter_with_timeout(
    program: &str,
    operation_flag: &'static str,
    input: &str,
    stdout_limit: usize,
) -> Result<Vec<u8>, String> {
    let operation = run_external_converter(program, operation_flag, input, stdout_limit);
    let timeout = async {
        smol::Timer::after(CONVERTER_TIMEOUT).await;
        Err("ID 转换器执行超过 2 秒，已终止".to_string())
    };
    smol::future::race(operation, timeout).await
}

async fn run_external_converter(
    program: &str,
    operation_flag: &str,
    input: &str,
    stdout_limit: usize,
) -> Result<Vec<u8>, String> {
    let mut command = std::process::Command::new(program);
    configure_no_window(&mut command);
    let mut command = smol::process::Command::from(command);
    command
        .arg(operation_flag)
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 ID 转换器失败：{error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ID 转换器未创建 stdout 管道".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ID 转换器未创建 stderr 管道".to_string())?;
    let (status, stdout, stderr) = futures::try_join!(
        child.status(),
        read_bounded(stdout, stdout_limit),
        read_bounded(stderr, MAX_CONVERTER_STDERR_BYTES),
    )
    .map_err(|error| format!("读取 ID 转换器结果失败：{error}"))?;

    if !status.success() {
        let detail = sanitize_stderr(&stderr);
        return Err(if detail.is_empty() {
            format!("ID 转换器退出状态异常：{status}")
        } else {
            format!("ID 转换器失败（{status}）：{detail}")
        });
    }
    Ok(stdout)
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

fn parse_integer_stdout(stdout: &[u8]) -> Result<i64, String> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| "ID 转换器 stdout 不是有效 UTF-8".to_string())?
        .trim();
    if text.is_empty() {
        return Err("ID 转换器 stdout 为空".to_string());
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("ID 转换器 stdout 必须只包含一个非负十进制整数".to_string());
    }
    let value = text
        .parse::<u64>()
        .map_err(|_| "ID 转换器输出的整数超出 u64 范围".to_string())?;
    i64::try_from(value).map_err(|_| "ID 转换器输出的整数超出 i64 范围".to_string())
}

fn parse_string_stdout(stdout: &[u8]) -> Result<String, String> {
    let text =
        std::str::from_utf8(stdout).map_err(|_| "ID 转换器 stdout 不是有效 UTF-8".to_string())?;
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if text.is_empty() {
        return Err("ID 转换器 stdout 为空".to_string());
    }
    if text.chars().any(char::is_control) {
        return Err("ID 转换器 stdout 必须只包含一行且不能包含控制字符".to_string());
    }
    Ok(text.to_string())
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
    use ramag_domain::entities::{IdConverterConfig, IdConverterKind};

    use super::{
        convert_id_to_integer, convert_id_to_string, parse_integer_stdout, parse_string_stdout,
        read_bounded, sanitize_stderr, validate_input,
    };

    #[test]
    fn converter_stdout_accepts_one_decimal_integer() {
        assert_eq!(
            parse_integer_stdout(b"2062113056040685562\n").unwrap(),
            2_062_113_056_040_685_562
        );
        assert_eq!(parse_integer_stdout(b"0").unwrap(), 0);
    }

    #[test]
    fn converter_stdout_rejects_non_decimal_and_overflow() {
        assert!(parse_integer_stdout(b"12 extra").is_err());
        assert!(parse_integer_stdout(b"-1").is_err());
        assert!(parse_integer_stdout(b"9223372036854775808").is_err());
        assert!(parse_integer_stdout(&[0xff]).is_err());
    }

    #[test]
    fn converter_string_stdout_accepts_one_utf8_line() {
        assert_eq!(parse_string_stdout(b"qwe\n").unwrap(), "qwe");
        assert_eq!(parse_string_stdout("测试".as_bytes()).unwrap(), "测试");
        assert_eq!(parse_string_stdout(b" value ").unwrap(), " value ");
    }

    #[test]
    fn converter_string_stdout_rejects_empty_multiline_and_control_text() {
        assert!(parse_string_stdout(b"").is_err());
        assert!(parse_string_stdout(b"first\nsecond").is_err());
        assert!(parse_string_stdout(b"value\0").is_err());
        assert!(parse_string_stdout(&[0xff]).is_err());
    }

    #[test]
    fn converter_stderr_is_safe_for_inline_display() {
        assert_eq!(sanitize_stderr(b"bad\n\x1b[31mvalue\0"), "bad [31mvalue");
    }

    #[test]
    fn converter_io_has_explicit_input_and_output_boundaries() {
        assert!(validate_input("").is_err());
        assert!(validate_input("line\nbreak").is_err());
        assert!(validate_input(&"x".repeat(4 * 1024 + 1)).is_err());

        let exact = smol::block_on(read_bounded(futures::io::Cursor::new(b"1234"), 4));
        assert_eq!(exact.unwrap(), b"1234");
        let oversized = smol::block_on(read_bounded(futures::io::Cursor::new(b"12345"), 4));
        assert!(oversized.is_err());
    }

    #[test]
    fn local_converter_uses_the_selected_preset() {
        let config = IdConverterConfig {
            kind: IdConverterKind::Base58Flickr,
            ..IdConverterConfig::default()
        };

        assert_eq!(
            smol::block_on(convert_id_to_integer(&config, "qwe")).unwrap(),
            82_489
        );
        assert_eq!(
            smol::block_on(convert_id_to_string(&config, "82489")).unwrap(),
            "qwe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_converter_receives_both_modes_and_one_argument() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_SCRIPT: AtomicU64 = AtomicU64::new(0);
        let suffix = NEXT_SCRIPT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ramag-id-converter-{}-{suffix}.sh",
            std::process::id()
        ));
        let script = r#"#!/bin/sh
if [ "$#" -ne 2 ]; then
  echo "unexpected argument count" >&2
  exit 9
fi
if [ "$1" = "-s" ] && [ "$2" = "value with spaces; echo injected" ]; then
  printf '%s\n' '2062113056040685562'
  exit 0
fi
if [ "$1" = "-i" ] && [ "$2" = "82489" ]; then
  printf '%s\n' 'qwe'
  exit 0
fi
echo "unexpected arguments" >&2
exit 9
"#;
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();

        let config = IdConverterConfig {
            kind: IdConverterKind::ExternalProgram,
            external_program: path.to_string_lossy().into_owned(),
            ..IdConverterConfig::default()
        };
        let integer = smol::block_on(convert_id_to_integer(
            &config,
            "value with spaces; echo injected",
        ));
        let string = smol::block_on(convert_id_to_string(&config, "00082489"));
        let _ = std::fs::remove_file(path);

        assert_eq!(integer.unwrap(), 2_062_113_056_040_685_562);
        assert_eq!(string.unwrap(), "qwe");
    }
}
