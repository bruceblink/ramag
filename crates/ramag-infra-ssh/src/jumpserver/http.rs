//! JumpServer HTTP 请求、响应限制与错误映射。

use super::*;

pub(super) fn authorized_request(
    request: reqwest::RequestBuilder,
    session: &JumpServerSession,
    org_id: Option<&str>,
) -> Result<reqwest::RequestBuilder> {
    let authorization =
        HeaderValue::from_str(&format!("{} {}", session.token_keyword, session.token))
            .map_err(|_| DomainError::ConnectionFailed("JumpServer 登录令牌无效".into()))?;
    let mut request = request
        .header(AUTHORIZATION, authorization)
        .header(ACCEPT, "application/json");
    if let Some(org_id) = org_id.filter(|id| !id.is_empty()) {
        let value = HeaderValue::from_str(org_id)
            .map_err(|_| DomainError::ConnectionFailed("JumpServer 组织 ID 无效".into()))?;
        request = request.header("x-jms-org", value);
    }
    Ok(request)
}

pub(super) async fn response_body(mut response: Response, operation: &str) -> Result<Vec<u8>> {
    let status = response.status();
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err(DomainError::ConnectionFailed(format!(
            "{operation}返回数据过大"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| request_error(operation, error))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(DomainError::ConnectionFailed(format!(
                "{operation}返回数据过大"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let message = serde_json::from_slice::<Value>(&body)
            .ok()
            .as_ref()
            .and_then(api_error_message)
            .unwrap_or_else(|| status_message(status));
        return Err(DomainError::ConnectionFailed(message));
    }
    Ok(body)
}

pub(super) fn request_error(operation: &str, error: reqwest::Error) -> DomainError {
    let details = request_error_details(&error);
    tracing::warn!(operation, error = %details, "JumpServer request failed");
    let reason = if error.is_timeout() {
        "请求超时".to_string()
    } else if error.is_connect() {
        classify_connection_error(&details).to_string()
    } else {
        error.to_string()
    };
    DomainError::ConnectionFailed(format!("{operation}失败：{reason}"))
}

fn request_error_details(error: &reqwest::Error) -> String {
    let mut details = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        details.push(' ');
        details.push_str(&cause.to_string());
        source = cause.source();
    }
    details
}

pub(super) fn classify_connection_error(details: &str) -> &'static str {
    let details = details.to_ascii_lowercase();
    if details.contains("certificate")
        || details.contains("unknownissuer")
        || details.contains("unknown issuer")
    {
        "TLS 证书校验失败"
    } else if details.contains("dns error")
        || details.contains("name resolution")
        || details.contains("failed to lookup address")
    {
        "域名解析失败"
    } else if details.contains("connection refused") {
        "服务器拒绝连接"
    } else if details.contains("network is unreachable") || details.contains("no route to host") {
        "网络不可达"
    } else if details.contains("operation not permitted") || details.contains("permission denied") {
        "系统阻止网络连接"
    } else if details.contains("connection reset")
        || details.contains("connection closed")
        || details.contains("broken pipe")
        || details.contains("unexpected eof")
    {
        "连接被网络或服务器中断"
    } else if details.contains("proxy") {
        "代理服务器连接失败"
    } else if details.contains("tls handshake") || details.contains("handshake failure") {
        "TLS 握手失败"
    } else {
        "无法连接服务器"
    }
}

fn status_message(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED => "JumpServer 用户名或密码错误，或登录已失效".into(),
        StatusCode::FORBIDDEN => "当前 JumpServer 账号无权访问该资源".into(),
        _ => format!("JumpServer 返回 HTTP {status}"),
    }
}

pub(super) fn api_error_message(value: &Value) -> Option<String> {
    for key in ["error", "msg", "detail", "message"] {
        if let Some(message) = value.get(key).and_then(Value::as_str) {
            let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
            if !message.is_empty() {
                return Some(message.chars().take(240).collect());
            }
        }
    }
    None
}

pub(super) fn parse_json<T: DeserializeOwned>(bytes: &[u8], operation: &str) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|error| DomainError::ConnectionFailed(format!("{operation}返回格式无效：{error}")))
}
