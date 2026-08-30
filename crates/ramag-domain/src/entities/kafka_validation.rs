use super::{
    MAX_KAFKA_BOOTSTRAP_SERVER_BYTES, MAX_KAFKA_MESSAGE_PREVIEW_BYTES, MAX_KAFKA_TOPIC_NAME_BYTES,
};

/// 校验 Kafka Topic 名称，并拒绝容易造成树节点歧义的保留名称。
pub fn validate_kafka_topic_name(name: &str) -> Result<(), String> {
    validate_required_text("Topic 名称", name, MAX_KAFKA_TOPIC_NAME_BYTES)?;
    if name == "." || name == ".." {
        return Err("Topic 名称不能是 . 或 ..".into());
    }
    Ok(())
}

/// 校验用户输入的 Bootstrap Server，要求 `host:port` 或 `[ipv6]:port`。
pub fn validate_kafka_bootstrap_server(server: &str) -> Result<(), String> {
    validate_required_text("Bootstrap Server", server, MAX_KAFKA_BOOTSTRAP_SERVER_BYTES)?;
    if server.contains("://") || server.contains(',') {
        return Err("Bootstrap Server 不能包含协议前缀或逗号".into());
    }

    let (host, port) = if let Some(rest) = server.strip_prefix('[') {
        let Some(close) = rest.find(']') else {
            return Err("IPv6 Bootstrap Server 缺少右方括号".into());
        };
        let host = &rest[..close];
        let Some(port) = rest
            .get(close + 1..)
            .and_then(|value| value.strip_prefix(':'))
        else {
            return Err("Bootstrap Server 必须包含端口".into());
        };
        (host, port)
    } else {
        let Some((host, port)) = server.rsplit_once(':') else {
            return Err("Bootstrap Server 必须使用 host:port 格式".into());
        };
        if host.is_empty() || host.contains(':') {
            return Err("IPv6 Bootstrap Server 必须使用 [host]:port 格式".into());
        }
        (host, port)
    };

    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err("Bootstrap Server 主机地址无效".into());
    }
    let _port = port
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| "Bootstrap Server 端口必须是 1 - 65535".to_string())?;
    Ok(())
}

/// 将原始 Key/Value 截取为有限的 UTF-8 宽度预览，二进制内容使用替换字符显示。
pub fn preview_bytes(bytes: &[u8], max_bytes: usize) -> super::KafkaTextPreview {
    if bytes.is_empty() {
        return super::KafkaTextPreview {
            text: String::new(),
            truncated: false,
        };
    }
    let max_bytes = max_bytes.min(MAX_KAFKA_MESSAGE_PREVIEW_BYTES);
    let take = bytes.len().min(max_bytes);
    let lossy = String::from_utf8_lossy(&bytes[..take]);
    let end = floor_char_boundary(&lossy, max_bytes);
    super::KafkaTextPreview {
        text: lossy[..end].to_string(),
        truncated: take < bytes.len() || end < lossy.len(),
    }
}

pub(super) fn validate_required_text(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label}不能为空"));
    }
    validate_single_line(label, value, max_bytes)
}

pub(super) fn validate_optional_single_line(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_single_line(label, value, max_bytes)?;
    }
    Ok(())
}

pub(super) fn validate_optional_protocol_text(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_protocol_text(label, value, max_bytes)?;
    }
    Ok(())
}

pub(super) fn validate_optional_path(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_required_text(label, value, max_bytes)?;
    }
    Ok(())
}

fn validate_single_line(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    validate_protocol_text(label, value, max_bytes)?;
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
}

pub(super) fn validate_protocol_text(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!(
            "{label}过长：{} bytes，最多 {max_bytes} bytes",
            value.len()
        ));
    }
    if value.contains('\0') {
        return Err(format!("{label}不能包含 NUL 字符"));
    }
    Ok(())
}

pub(super) fn validate_optional_offset(offset: Option<i64>) -> Result<(), String> {
    if offset.is_some_and(|offset| offset < 0) {
        return Err("Offset 不能为负数".into());
    }
    Ok(())
}

pub(super) fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}
