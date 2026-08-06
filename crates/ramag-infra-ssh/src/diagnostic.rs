//! 无 PTY、固定模板的 Linux / Windows 生产诊断执行器。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::json;

use ramag_domain::entities::{
    DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS, DiagnosticCancellation, DiagnosticTermination,
    MAX_DIAGNOSTIC_ITEMS, MAX_DIAGNOSTIC_OUTPUT_BYTES, RemoteOperatingSystem,
    RemotePlatformPreference, RemoteShellKind, SshDiagnosticOperation, SshDiagnosticProviderKind,
    SshDiagnosticResult, SshLogSource, SshProfile, SshRemoteCapabilities, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};

use crate::askpass::AskPassBroker;
use crate::command::{
    OpenSshLocator, diagnostic_args, powershell_encoded_command, terminal_probe_args,
};

mod process;

use process::execute_process;

const LINUX_PROBE: &str = "LC_ALL=C PATH=/usr/bin:/bin; os=$(uname -s 2>/dev/null) || exit 64; case \"$os\" in Linux*) printf 'ramag-os=linux-v1\\n';; *) exit 65;; esac; printf '%s\\n' \"$os\"";
const LINUX_SYSTEM_OVERVIEW: &str = "LC_ALL=C PATH=/usr/bin:/bin; printf 'section=kernel\\n'; uname -a; printf 'section=os-release\\n'; if [ -r /etc/os-release ]; then sed -n '1,40p' /etc/os-release; fi; printf 'section=uptime\\n'; uptime";
const LINUX_RESOURCE_SNAPSHOT: &str = "LC_ALL=C PATH=/usr/bin:/bin; printf 'section=cpu\\n'; getconf _NPROCESSORS_ONLN; printf 'section=load\\n'; sed -n '1p' /proc/loadavg; printf 'section=memory\\n'; sed -n '1,64p' /proc/meminfo";
const LINUX_PROCESS_LIST: &str =
    "LC_ALL=C PATH=/usr/bin:/bin; exec ps -eo pid=,user=,pcpu=,pmem=,comm=";
const LINUX_NETWORK_SNAPSHOT: &str =
    "LC_ALL=C PATH=/usr/sbin:/usr/bin:/sbin:/bin; exec ss -H -lntu";
const LINUX_DISK_OVERVIEW: &str = "LC_ALL=C PATH=/usr/bin:/bin; exec df -Pk";
const LINUX_SERVICE_STATUS: &str = "LC_ALL=C PATH=/usr/bin:/bin; IFS= read -r name || exit 64; case \"$name\" in ''|*[!A-Za-z0-9._@-]*) exit 64;; esac; exec systemctl --no-pager --full status -- \"$name\"";
const LINUX_LOG_QUERY: &str = "LC_ALL=C PATH=/usr/bin:/bin; IFS= read -r source || exit 64; IFS= read -r max || exit 64; IFS= read -r minutes || exit 64; IFS= read -r service || exit 64; case \"$max\" in ''|*[!0-9]*) exit 64;; esac; case \"$minutes\" in ''|*[!0-9]*) exit 64;; esac; [ \"$max\" -ge 1 ] 2>/dev/null && [ \"$max\" -le 5000 ] 2>/dev/null || exit 64; [ \"$minutes\" -ge 1 ] 2>/dev/null && [ \"$minutes\" -le 1440 ] 2>/dev/null || exit 64; case \"$source\" in system) exec journalctl --no-pager -n \"$max\" --since \"-$minutes min\";; service) case \"$service\" in ''|*[!A-Za-z0-9._@-]*) exit 64;; esac; exec journalctl --no-pager -n \"$max\" --since \"-$minutes min\" --unit \"$service\";; *) exit 64;; esac";

// 固定脚本只接受受限 JSON；EncodedCommand 内容不随用户输入变化。
const WINDOWS_PROVIDER: &str = r#"$ErrorActionPreference='Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'not_windows' }
[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false)
$raw=[Console]::In.ReadToEnd()
if ($raw.Length -gt 16384) { throw 'request_too_large' }
$r=$raw | ConvertFrom-Json
switch ([string]$r.operation) {
  'probe' {
    $shell='cmd'
    try {
      $configured=[IO.Path]::GetFileName((Get-ItemProperty -LiteralPath 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\OpenSSH' -Name DefaultShell -ErrorAction Stop).DefaultShell).ToLowerInvariant()
      if ($configured -eq 'powershell.exe') { $shell='windows_powershell' }
      elseif ($configured -eq 'pwsh.exe') { $shell='powershell_core' }
      elseif ($configured -ne 'cmd.exe') { $shell='unknown' }
    } catch {}
    $home=[Environment]::GetFolderPath('UserProfile').Replace('\','/')
    $result=[ordered]@{protocol='ramag-windows-v1'; version=[Environment]::OSVersion.VersionString; shell=$shell; home=$home}
  }
  'system_overview' {
    $os=Get-CimInstance Win32_OperatingSystem
    $result=[ordered]@{computer=$env:COMPUTERNAME; version=[Environment]::OSVersion.VersionString; caption=$os.Caption; lastBoot=$os.LastBootUpTime; localTime=[DateTime]::Now}
  }
  'resource_snapshot' {
    $os=Get-CimInstance Win32_OperatingSystem
    $cpu=@(Get-CimInstance Win32_Processor | ForEach-Object { [ordered]@{name=$_.Name; logicalProcessors=$_.NumberOfLogicalProcessors; loadPercent=$_.LoadPercentage} })
    $result=[ordered]@{totalMemoryKiB=$os.TotalVisibleMemorySize; freeMemoryKiB=$os.FreePhysicalMemory; processors=$cpu}
  }
  'process_list' {
    $result=@(Get-Process | Select-Object -First 5000 | ForEach-Object { [ordered]@{pid=$_.Id; name=$_.ProcessName; cpuSeconds=$_.CPU; workingSetBytes=$_.WorkingSet64} })
  }
  'network_snapshot' {
    $result=@(Get-NetTCPConnection | Select-Object -First 5000 | ForEach-Object { [ordered]@{localAddress=$_.LocalAddress; localPort=$_.LocalPort; remoteAddress=$_.RemoteAddress; remotePort=$_.RemotePort; state=[string]$_.State; owningPid=$_.OwningProcess} })
  }
  'disk_overview' {
    $result=@(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | ForEach-Object { [ordered]@{deviceId=$_.DeviceID; volumeName=$_.VolumeName; sizeBytes=$_.Size; freeBytes=$_.FreeSpace; fileSystem=$_.FileSystem} })
  }
  'service_status' {
    $s=Get-Service -Name ([string]$r.service) -ErrorAction Stop
    $result=[ordered]@{name=$s.Name; displayName=$s.DisplayName; status=[string]$s.Status; startType=[string]$s.StartType}
  }
  'log_query' {
    if ($r.source -notin @('System','Application')) { throw 'unsupported_log_source' }
    $start=[DateTime]::Now.AddMinutes(-[int]$r.minutes)
    $result=@(Get-WinEvent -FilterHashtable @{LogName=[string]$r.source; StartTime=$start} -MaxEvents ([int]$r.maxItems) | ForEach-Object { [ordered]@{id=$_.Id; level=$_.LevelDisplayName; provider=$_.ProviderName; timeCreated=$_.TimeCreated} })
  }
  default { throw 'unsupported_operation' }
}
$result | ConvertTo-Json -Compress -Depth 5"#;

pub(crate) struct RemotePlatformProbe {
    pub operating_system: RemoteOperatingSystem,
    pub shell: RemoteShellKind,
    pub provider: SshDiagnosticProviderKind,
    pub default_directory_hint: Option<String>,
}

pub(crate) async fn probe_operating_system(
    locator: &OpenSshLocator,
    askpass: &AskPassBroker,
    profile: &SshProfile,
) -> Result<RemotePlatformProbe> {
    let capability = locator.probe(profile.ssh_path.clone()).await?;
    let windows_command = windows_remote_command();
    let probes = match profile.remote_platform {
        RemotePlatformPreference::Auto | RemotePlatformPreference::Linux => [
            (RemoteOperatingSystem::Linux, LINUX_PROBE, Vec::new()),
            (
                RemoteOperatingSystem::Windows,
                windows_command.as_str(),
                br#"{"operation":"probe"}"#.to_vec(),
            ),
        ],
        RemotePlatformPreference::Windows => [
            (
                RemoteOperatingSystem::Windows,
                windows_command.as_str(),
                br#"{"operation":"probe"}"#.to_vec(),
            ),
            (RemoteOperatingSystem::Linux, LINUX_PROBE, Vec::new()),
        ],
    };
    let mut last_error = None;
    for (operating_system, command, input) in probes {
        let execution = execute_process(
            &capability.executable,
            diagnostic_args(profile, command)?,
            askpass.environment(profile)?,
            input,
            Duration::from_secs(5),
            DiagnosticCancellation::default(),
        )
        .await;
        match execution {
            Ok(execution) if execution.exit_code == Some(0) => {
                let windows_details = (operating_system == RemoteOperatingSystem::Windows)
                    .then(|| parse_windows_probe(&execution.stdout))
                    .flatten();
                let matched = (operating_system == RemoteOperatingSystem::Linux
                    && String::from_utf8_lossy(&execution.stdout).contains("ramag-os=linux-v1"))
                    || windows_details.is_some();
                if !matched {
                    last_error = Some(execution.bounded_error());
                    continue;
                }
                let provider = match operating_system {
                    RemoteOperatingSystem::Linux => SshDiagnosticProviderKind::LinuxBuiltinV1,
                    RemoteOperatingSystem::Windows => {
                        SshDiagnosticProviderKind::WindowsPowerShellV1
                    }
                    RemoteOperatingSystem::Unknown => {
                        return Err(DomainError::Other("内部平台探测返回了未知平台".into()));
                    }
                };
                let (shell, default_directory_hint) =
                    windows_details.map_or((RemoteShellKind::Posix, None), |details| details);
                return Ok(RemotePlatformProbe {
                    operating_system,
                    shell,
                    provider,
                    default_directory_hint,
                });
            }
            Ok(execution) => last_error = Some(execution.bounded_error()),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(DomainError::ConnectionFailed(format!(
        "无法识别远端 Linux / Windows 平台：{}",
        last_error.unwrap_or_else(|| "远端未返回平台标记".into())
    )))
}

fn parse_windows_probe(bytes: &[u8]) -> Option<(RemoteShellKind, Option<String>)> {
    // JumpServer 等堡垒机可能在固定 JSON 前输出欢迎语；只接受完整单行协议对象。
    let value = bytes
        .split(|byte| *byte == b'\n')
        .rev()
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = line.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(line);
            serde_json::from_slice::<serde_json::Value>(line).ok()
        })
        .find(|value| {
            value.get("protocol").and_then(serde_json::Value::as_str) == Some("ramag-windows-v1")
        })?;
    if value.get("protocol")?.as_str()? != "ramag-windows-v1" {
        return None;
    }
    let shell = match value.get("shell").and_then(serde_json::Value::as_str) {
        Some("cmd") => RemoteShellKind::Cmd,
        Some("windows_powershell") => RemoteShellKind::WindowsPowerShell,
        Some("powershell_core") => RemoteShellKind::PowerShellCore,
        _ => RemoteShellKind::Unknown,
    };
    let home = value
        .get("home")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|home| !home.is_empty())
        .filter(|home| validate_remote_path(home).is_ok())
        .map(str::to_string);
    Some((shell, home))
}

pub(crate) async fn probe_interactive_terminal(
    locator: &OpenSshLocator,
    askpass: &AskPassBroker,
    profile: &SshProfile,
) -> Result<()> {
    let capability = locator.probe(profile.ssh_path.clone()).await?;
    let execution = execute_process(
        &capability.executable,
        terminal_probe_args(profile)?,
        askpass.environment(profile)?,
        Vec::new(),
        Duration::from_secs(5),
        DiagnosticCancellation::default(),
    )
    .await?;
    if execution.exit_code == Some(0) && execution.termination == DiagnosticTermination::Completed {
        Ok(())
    } else {
        Err(DomainError::ConnectionFailed(format!(
            "交互式 Terminal 探测失败：{}",
            execution.bounded_error()
        )))
    }
}

pub(crate) async fn execute(
    locator: &OpenSshLocator,
    askpass_env: HashMap<String, String>,
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    operation: &SshDiagnosticOperation,
    cancellation: DiagnosticCancellation,
) -> Result<SshDiagnosticResult> {
    operation.validate().map_err(DomainError::InvalidConfig)?;
    let capability = locator.probe(profile.ssh_path.clone()).await?;
    let provider = capabilities
        .diagnostic_provider
        .ok_or_else(|| DomainError::Forbidden("当前连接没有可用的安全诊断提供者".into()))?;
    let (command, input, timeout) = operation_command(provider, operation)?;
    let started = Instant::now();
    let execution = execute_process(
        &capability.executable,
        diagnostic_args(profile, &command)?,
        askpass_env,
        input,
        timeout,
        cancellation,
    )
    .await?;
    let (output, line_truncated) = if provider == SshDiagnosticProviderKind::WindowsPowerShellV1
        && execution.exit_code == Some(0)
        && execution.termination == DiagnosticTermination::Completed
    {
        (validate_windows_json(&execution.stdout)?, false)
    } else {
        bounded_visible_output(&execution.stdout, &execution.stderr)
    };
    Ok(SshDiagnosticResult {
        profile_id: profile.id.clone(),
        operation: operation.kind().into(),
        operating_system: capabilities.operating_system,
        provider,
        output,
        exit_code: execution.exit_code,
        termination: execution.termination,
        truncated: execution.truncated || line_truncated,
        elapsed_millis: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

fn operation_command(
    provider: SshDiagnosticProviderKind,
    operation: &SshDiagnosticOperation,
) -> Result<(String, Vec<u8>, Duration)> {
    match provider {
        SshDiagnosticProviderKind::LinuxBuiltinV1 => linux_operation(operation),
        SshDiagnosticProviderKind::WindowsPowerShellV1 => windows_operation(operation),
        SshDiagnosticProviderKind::GatewayV1 => Err(DomainError::NotImplemented(
            "当前客户端尚未配置版本化诊断网关".into(),
        )),
    }
}

fn linux_operation(operation: &SshDiagnosticOperation) -> Result<(String, Vec<u8>, Duration)> {
    let five = Duration::from_secs(5);
    let ten = Duration::from_secs(DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS);
    let value = match operation {
        SshDiagnosticOperation::SystemOverview => (LINUX_SYSTEM_OVERVIEW, Vec::new(), five),
        SshDiagnosticOperation::ResourceSnapshot => (LINUX_RESOURCE_SNAPSHOT, Vec::new(), five),
        SshDiagnosticOperation::ProcessList => (LINUX_PROCESS_LIST, Vec::new(), ten),
        SshDiagnosticOperation::NetworkSnapshot => (LINUX_NETWORK_SNAPSHOT, Vec::new(), ten),
        SshDiagnosticOperation::DiskOverview => (LINUX_DISK_OVERVIEW, Vec::new(), five),
        SshDiagnosticOperation::ServiceStatus { name } => (
            LINUX_SERVICE_STATUS,
            format!("{}\n", name.as_str()).into_bytes(),
            five,
        ),
        SshDiagnosticOperation::LogQuery {
            source,
            service,
            max_items,
            since,
        } => {
            if *source == SshLogSource::Application {
                return Err(DomainError::NotImplemented(
                    "Linux 诊断不支持 Application 事件日志源".into(),
                ));
            }
            let source = match source {
                SshLogSource::System => "system",
                SshLogSource::Service => "service",
                SshLogSource::Application => {
                    return Err(DomainError::NotImplemented(
                        "Linux 诊断不支持 Application 事件日志源".into(),
                    ));
                }
            };
            let minutes = since.map_or(60, |range| range.minutes());
            let service = service.as_ref().map_or("", |name| name.as_str());
            (
                LINUX_LOG_QUERY,
                format!("{source}\n{max_items}\n{minutes}\n{service}\n").into_bytes(),
                ten,
            )
        }
        SshDiagnosticOperation::FileMetadata { .. } | SshDiagnosticOperation::FileChunk { .. } => {
            return Err(DomainError::NotImplemented(
                "文件诊断必须通过结构化 SFTP 执行".into(),
            ));
        }
    };
    Ok((value.0.into(), value.1, value.2))
}

fn windows_operation(operation: &SshDiagnosticOperation) -> Result<(String, Vec<u8>, Duration)> {
    let (request, timeout) = match operation {
        SshDiagnosticOperation::SystemOverview => (
            json!({"operation": "system_overview"}),
            Duration::from_secs(5),
        ),
        SshDiagnosticOperation::ResourceSnapshot => (
            json!({"operation": "resource_snapshot"}),
            Duration::from_secs(5),
        ),
        SshDiagnosticOperation::ProcessList => (
            json!({"operation": "process_list"}),
            Duration::from_secs(DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS),
        ),
        SshDiagnosticOperation::NetworkSnapshot => (
            json!({"operation": "network_snapshot"}),
            Duration::from_secs(DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS),
        ),
        SshDiagnosticOperation::DiskOverview => (
            json!({"operation": "disk_overview"}),
            Duration::from_secs(5),
        ),
        SshDiagnosticOperation::ServiceStatus { name } => (
            json!({"operation": "service_status", "service": name.as_str()}),
            Duration::from_secs(5),
        ),
        SshDiagnosticOperation::LogQuery {
            source,
            max_items,
            since,
            service,
        } => {
            if service.is_some() || *source == SshLogSource::Service {
                return Err(DomainError::NotImplemented(
                    "Windows 首版只支持 System 和 Application 事件日志".into(),
                ));
            }
            let source = match source {
                SshLogSource::System => "System",
                SshLogSource::Application => "Application",
                SshLogSource::Service => {
                    return Err(DomainError::NotImplemented(
                        "Windows 首版不支持按服务读取事件日志".into(),
                    ));
                }
            };
            (
                json!({
                    "operation": "log_query",
                    "source": source,
                    "maxItems": max_items,
                    "minutes": since.map_or(60, |range| range.minutes()),
                }),
                Duration::from_secs(DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS),
            )
        }
        SshDiagnosticOperation::FileMetadata { .. } | SshDiagnosticOperation::FileChunk { .. } => {
            return Err(DomainError::NotImplemented(
                "文件诊断必须通过结构化 SFTP 执行".into(),
            ));
        }
    };
    let input = serde_json::to_vec(&request)
        .map_err(|error| DomainError::Other(format!("序列化诊断请求失败：{error}")))?;
    Ok((windows_remote_command(), input, timeout))
}

fn windows_remote_command() -> String {
    powershell_encoded_command(WINDOWS_PROVIDER)
}

fn bounded_visible_output(stdout: &[u8], stderr: &[u8]) -> (String, bool) {
    let primary = if stdout.is_empty() { stderr } else { stdout };
    let value = String::from_utf8_lossy(primary);
    let mut result = String::with_capacity(value.len().min(MAX_DIAGNOSTIC_OUTPUT_BYTES));
    let mut lines = 0usize;
    let mut truncated = false;
    for character in value.chars() {
        if result.len() >= MAX_DIAGNOSTIC_OUTPUT_BYTES || lines >= MAX_DIAGNOSTIC_ITEMS {
            truncated = true;
            break;
        }
        match character {
            '\n' => {
                result.push('\n');
                lines += 1;
            }
            '\r' | '\t' => result.push(character),
            character if !character.is_control() => result.push(character),
            _ => {}
        }
    }
    (result, truncated)
}

fn validate_windows_json(bytes: &[u8]) -> Result<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        DomainError::ConnectionFailed(format!("Windows 诊断返回了无效 JSON：{error}"))
    })?;
    let mut nodes = 0usize;
    validate_json_value(&value, 0, &mut nodes)?;
    serde_json::to_string(&value)
        .map_err(|error| DomainError::Other(format!("规范化 Windows 诊断 JSON 失败：{error}")))
}

fn validate_json_value(value: &serde_json::Value, depth: usize, nodes: &mut usize) -> Result<()> {
    if depth > 8 {
        return Err(DomainError::ConnectionFailed(
            "Windows 诊断 JSON 嵌套过深".into(),
        ));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_DIAGNOSTIC_ITEMS.saturating_mul(16) {
        return Err(DomainError::ConnectionFailed(
            "Windows 诊断 JSON 节点过多".into(),
        ));
    }
    match value {
        serde_json::Value::Array(values) => {
            if values.len() > MAX_DIAGNOSTIC_ITEMS {
                return Err(DomainError::ConnectionFailed(
                    "Windows 诊断 JSON 项目超过上限".into(),
                ));
            }
            for value in values {
                validate_json_value(value, depth + 1, nodes)?;
            }
        }
        serde_json::Value::Object(values) => {
            if values.len() > 128 {
                return Err(DomainError::ConnectionFailed(
                    "Windows 诊断 JSON 字段过多".into(),
                ));
            }
            for (key, value) in values {
                if key.len() > 128 {
                    return Err(DomainError::ConnectionFailed(
                        "Windows 诊断 JSON 字段名过长".into(),
                    ));
                }
                validate_json_value(value, depth + 1, nodes)?;
            }
        }
        serde_json::Value::String(value) if value.len() > 64 * 1024 => {
            return Err(DomainError::ConnectionFailed(
                "Windows 诊断 JSON 文本字段过长".into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{DiagnosticTimeRange, SshServiceName};

    #[test]
    fn windows_encoded_launcher_is_fixed_and_contains_no_request_values() {
        let command = windows_remote_command();
        assert!(
            command
                .starts_with("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ")
        );
        assert!(!command.contains("dangerous-service"));
        assert!(!command.contains("Invoke-Expression"));
        assert!(!command.contains("ExecutionPolicy"));
    }

    #[test]
    fn windows_probe_returns_shell_and_bounded_home_hint() {
        let (shell, home) = parse_windows_probe(
            b"Welcome to JumpServer SSH Server\r\n\
              Connecting to administrator@10.0.0.1\r\n\
              {\"protocol\":\"ramag-windows-v1\",\"shell\":\"cmd\",\"home\":\"C:/Users/Administrator\"}\r\n",
        )
        .unwrap();
        assert_eq!(shell, RemoteShellKind::Cmd);
        assert_eq!(home.as_deref(), Some("C:/Users/Administrator"));
        assert!(parse_windows_probe(br#"{"protocol":"other"}"#).is_none());
    }

    #[test]
    fn windows_request_keeps_service_name_out_of_command() {
        let operation = SshDiagnosticOperation::ServiceStatus {
            name: SshServiceName::parse("dangerous-service").unwrap(),
        };
        let (command, input, _) = windows_operation(&operation).unwrap();
        assert!(!command.contains("dangerous-service"));
        assert!(
            String::from_utf8(input)
                .unwrap()
                .contains("dangerous-service")
        );
    }

    #[test]
    fn linux_parameters_use_stdin_not_remote_command() {
        let operation = SshDiagnosticOperation::LogQuery {
            source: SshLogSource::Service,
            service: Some(SshServiceName::parse("ssh.service").unwrap()),
            max_items: 50,
            since: Some(DiagnosticTimeRange::last_minutes(30).unwrap()),
        };
        let (command, input, _) = linux_operation(&operation).unwrap();
        assert!(!command.contains("ssh.service"));
        assert!(String::from_utf8(input).unwrap().contains("ssh.service"));
    }

    #[test]
    fn visible_output_removes_terminal_controls_and_bounds_lines() {
        let (output, truncated) = bounded_visible_output(b"safe\x1b[31m\nnext", b"");
        assert_eq!(output, "safe[31m\nnext");
        assert!(!truncated);
    }

    #[test]
    fn windows_json_rejects_deep_or_oversized_protocol_values() {
        assert!(validate_windows_json(br#"{"ok":true}"#).is_ok());
        let deep = br#"[[[[[[[[[[1]]]]]]]]]]"#;
        assert!(validate_windows_json(deep).is_err());
    }
}
