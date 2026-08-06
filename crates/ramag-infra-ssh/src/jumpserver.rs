//! JumpServer Bearer 认证与当前用户授权资产 API。

use std::collections::HashSet;
use std::error::Error as StdError;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, HeaderValue};
use reqwest::{Client, Response, StatusCode, Url, redirect};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use ramag_domain::entities::{
    JumpServerAccount, JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog,
    JumpServerCredential, JumpServerLabel, JumpServerNode, JumpServerOrganization,
    JumpServerSession, MAX_JUMPSERVER_ASSETS, MAX_JUMPSERVER_NODES, MAX_JUMPSERVER_TOKEN_BYTES,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::JumpServerDriver;

use crate::runtime::run_in_tokio;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const ASSET_PAGE_SIZE: usize = 100;
const MAX_ASSET_PAGES: usize = 1000;
const NODE_PAGE_SIZE: usize = 200;
const MAX_NODE_PAGES: usize = 1000;
const MAX_API_FIELD_BYTES: usize = 4096;

#[derive(Clone)]
pub struct JumpServerHttpDriver {
    client: Client,
}

impl JumpServerHttpDriver {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(REQUEST_TIMEOUT)
            .redirect(redirect::Policy::none())
            // JumpServer 多为内网服务；固定直连可避免工作区其它依赖意外启用系统代理。
            .no_proxy()
            .user_agent(concat!("Ramag/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                DomainError::Other(format!("创建 JumpServer HTTP 客户端失败：{error}"))
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl JumpServerDriver for JumpServerHttpDriver {
    async fn authenticate(&self, credential: &JumpServerCredential) -> Result<JumpServerSession> {
        credential.validate().map_err(DomainError::InvalidConfig)?;
        let client = self.client.clone();
        let credential = credential.clone();
        run_in_tokio(async move { authenticate(&client, credential).await }).await
    }

    async fn load_catalog(&self, session: &JumpServerSession) -> Result<JumpServerCatalog> {
        let client = self.client.clone();
        let session = session.clone();
        run_in_tokio(async move { load_catalog(&client, &session).await }).await
    }

    async fn asset_detail(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail> {
        let client = self.client.clone();
        let session = session.clone();
        let asset = asset.clone();
        run_in_tokio(async move { asset_detail(&client, &session, &asset).await }).await
    }

    async fn create_rdp_web_session(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
        account: &JumpServerAccount,
    ) -> Result<String> {
        let client = self.client.clone();
        let session = session.clone();
        let asset = asset.clone();
        let account = account.clone();
        run_in_tokio(
            async move { create_rdp_web_session(&client, &session, &asset, &account).await },
        )
        .await
    }
}

#[derive(Debug)]
struct Endpoint {
    base_url: String,
    ssh_host: String,
}

fn normalize_endpoint(input: &str) -> Result<Endpoint> {
    let trimmed = input.trim();
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = Url::parse(&candidate)
        .map_err(|error| DomainError::InvalidConfig(format!("JumpServer 地址无效：{error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(DomainError::InvalidConfig(
            "JumpServer 地址只支持 http 或 https".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(DomainError::InvalidConfig(
            "JumpServer 地址不能包含用户名或密码".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(DomainError::InvalidConfig(
            "JumpServer 地址不能包含查询参数或片段".into(),
        ));
    }
    let ssh_host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| DomainError::InvalidConfig("JumpServer 地址缺少主机名".into()))?
        .to_string();
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(Endpoint {
        base_url: url.to_string(),
        ssh_host,
    })
}

fn api_url(base_url: &str, path: &str) -> Result<Url> {
    Url::parse(base_url)
        .and_then(|base| base.join(path))
        .map_err(|error| DomainError::InvalidConfig(format!("JumpServer API 地址无效：{error}")))
}

#[derive(Deserialize)]
struct LoginSuccess {
    token: String,
    #[serde(default = "default_token_keyword")]
    keyword: String,
    user: LoginUser,
}

#[derive(Deserialize)]
struct LoginUser {
    username: String,
    #[serde(default)]
    workbench_orgs: Vec<LoginOrganization>,
}

#[derive(Deserialize)]
struct LoginOrganization {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct UserPermissions {
    #[serde(default)]
    workbench_orgs: Vec<LoginOrganization>,
}

fn default_token_keyword() -> String {
    "Bearer".into()
}

async fn authenticate(
    client: &Client,
    credential: JumpServerCredential,
) -> Result<JumpServerSession> {
    let endpoint = normalize_endpoint(&credential.base_url)?;
    let url = api_url(&endpoint.base_url, "api/v1/authentication/auth/")?;
    let response = client
        .post(url)
        .json(&serde_json::json!({
            "username": credential.username,
            "password": credential.password,
        }))
        .send()
        .await
        .map_err(|error| request_error("登录 JumpServer", error))?;
    let body = response_body(response, "登录 JumpServer").await?;
    let value: Value = parse_json(&body, "登录 JumpServer")?;
    let success: LoginSuccess = serde_json::from_value(value.clone()).map_err(|_| {
        let detail = api_error_message(&value)
            .unwrap_or_else(|| "当前账号可能需要 MFA 或登录确认，本版本暂不支持二次认证".into());
        DomainError::ConnectionFailed(detail)
    })?;
    if success.token.is_empty() || success.token.len() > MAX_JUMPSERVER_TOKEN_BYTES {
        return Err(DomainError::ConnectionFailed(
            "JumpServer 返回的登录令牌无效".into(),
        ));
    }
    if !success.keyword.eq_ignore_ascii_case("Bearer") {
        return Err(DomainError::ConnectionFailed(
            "JumpServer 返回了不支持的认证类型".into(),
        ));
    }
    let canonical_username = success.user.username.trim().to_string();
    let canonical_credential = JumpServerCredential {
        base_url: endpoint.base_url.clone(),
        ssh_port: credential.ssh_port,
        username: canonical_username.clone(),
        password: credential.password.clone(),
    };
    canonical_credential
        .validate()
        .map_err(DomainError::InvalidConfig)?;

    let mut session = JumpServerSession {
        base_url: endpoint.base_url,
        ssh_host: endpoint.ssh_host,
        ssh_port: credential.ssh_port,
        username: canonical_username,
        password: credential.password,
        token_keyword: "Bearer".into(),
        token: success.token,
        organizations: validate_organizations(success.user.workbench_orgs)?,
    };
    if session.organizations.is_empty() {
        session.organizations = fetch_workbench_organizations(client, &session).await?;
    }
    Ok(session)
}

async fn fetch_workbench_organizations(
    client: &Client,
    session: &JumpServerSession,
) -> Result<Vec<JumpServerOrganization>> {
    let url = api_url(&session.base_url, "api/v1/users/profile/permissions/")?;
    let response = authorized_request(client.get(url), session, None)?
        .send()
        .await
        .map_err(|error| request_error("获取 JumpServer 组织权限", error))?;
    // 较老版本可能没有该接口，此时继续使用默认组织读取资源。
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let body = response_body(response, "获取 JumpServer 组织权限").await?;
    let permissions: UserPermissions = parse_json(&body, "获取 JumpServer 组织权限")?;
    validate_organizations(permissions.workbench_orgs)
}

fn validate_organizations(
    organizations: Vec<LoginOrganization>,
) -> Result<Vec<JumpServerOrganization>> {
    let mut seen = HashSet::new();
    let mut validated = Vec::new();
    for organization in organizations {
        let id = checked_field("组织 ID", organization.id)?;
        HeaderValue::from_str(&id)
            .map_err(|_| DomainError::ConnectionFailed("JumpServer 组织 ID 无效".into()))?;
        if seen.insert(id.clone()) {
            validated.push(JumpServerOrganization {
                id,
                name: checked_field("组织名称", organization.name)?,
            });
        }
    }
    Ok(validated)
}

async fn load_catalog(client: &Client, session: &JumpServerSession) -> Result<JumpServerCatalog> {
    let nodes = list_nodes(client, session).await?;
    let mut assets = list_assets(client, session).await?;
    if !nodes.is_empty() {
        mark_special_assets(client, session, &nodes, &mut assets, "favorite").await?;
        mark_special_assets(client, session, &nodes, &mut assets, "ungrouped").await?;
    }
    Ok(JumpServerCatalog { assets, nodes })
}

async fn list_nodes(client: &Client, session: &JumpServerSession) -> Result<Vec<JumpServerNode>> {
    let organizations: Vec<Option<JumpServerOrganization>> = if session.organizations.is_empty() {
        vec![None]
    } else {
        session.organizations.iter().cloned().map(Some).collect()
    };
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    for organization in organizations {
        let org_id = organization.as_ref().map(|org| org.id.as_str());
        if !list_organization_nodes(client, session, org_id, &mut nodes, &mut seen).await? {
            tracing::info!("JumpServer node API is unavailable; using flat asset list");
            return Ok(Vec::new());
        }
    }
    nodes.sort_by(|left, right| {
        left.org_id
            .cmp(&right.org_id)
            .then_with(|| special_node_order(left).cmp(&special_node_order(right)))
            .then_with(|| left.key.cmp(&right.key))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(nodes)
}

async fn list_organization_nodes(
    client: &Client,
    session: &JumpServerSession,
    org_id: Option<&str>,
    nodes: &mut Vec<JumpServerNode>,
    seen: &mut HashSet<(String, String)>,
) -> Result<bool> {
    let mut offset = 0usize;
    let mut pages = 0usize;
    loop {
        let mut url = api_url(&session.base_url, "api/v1/perms/users/self/nodes/")?;
        url.query_pairs_mut()
            .append_pair("limit", &NODE_PAGE_SIZE.to_string())
            .append_pair("offset", &offset.to_string());
        let response = authorized_request(client.get(url), session, org_id)?
            .send()
            .await
            .map_err(|error| request_error("获取 JumpServer 资产树", error))?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Ok(false);
        }
        let body = response_body(response, "获取 JumpServer 资产树").await?;
        let (wire_nodes, count, paged) = parse_node_page(&body)?;
        if count.is_some_and(|total| total > MAX_JUMPSERVER_NODES) {
            return Err(DomainError::Other(format!(
                "JumpServer 资产节点超过 {MAX_JUMPSERVER_NODES} 条上限，请缩小授权范围"
            )));
        }
        let page_len = wire_nodes.len();
        let seen_before = seen.len();
        for wire in wire_nodes {
            let node = wire.into_node(org_id.unwrap_or_default())?;
            let identity = (node.org_id.clone(), node.id.clone());
            if seen.insert(identity) {
                if nodes.len() >= MAX_JUMPSERVER_NODES {
                    return Err(DomainError::Other(format!(
                        "JumpServer 资产节点超过 {MAX_JUMPSERVER_NODES} 条上限，请缩小授权范围"
                    )));
                }
                nodes.push(node);
            }
        }
        offset = offset.saturating_add(page_len);
        pages = pages.saturating_add(1);
        if page_len == 0 || !paged || count.is_some_and(|total| offset >= total) {
            break;
        }
        if seen.len() == seen_before || pages >= MAX_NODE_PAGES {
            return Err(DomainError::ConnectionFailed(
                "JumpServer 资产树分页异常，请稍后重试".into(),
            ));
        }
    }
    Ok(true)
}

async fn mark_special_assets(
    client: &Client,
    session: &JumpServerSession,
    nodes: &[JumpServerNode],
    assets: &mut [JumpServerAsset],
    special_key: &str,
) -> Result<()> {
    let organizations: Vec<Option<String>> = if session.organizations.is_empty() {
        vec![None]
    } else {
        session
            .organizations
            .iter()
            .map(|organization| Some(organization.id.clone()))
            .collect()
    };
    for organization in organizations {
        let org_id = organization.as_deref();
        let has_special_assets = nodes.iter().any(|node| {
            node.org_id == org_id.unwrap_or_default()
                && node.key == special_key
                && node.assets_amount > 0
        });
        if !has_special_assets {
            continue;
        }
        let path = format!("api/v1/perms/users/self/nodes/{special_key}/assets/");
        let mut special_assets = Vec::new();
        let mut seen = HashSet::new();
        list_organization_assets(
            client,
            session,
            org_id,
            &path,
            &mut special_assets,
            &mut seen,
        )
        .await?;
        for asset in assets
            .iter_mut()
            .filter(|asset| asset.org_id == org_id.unwrap_or_default() && seen.contains(&asset.id))
        {
            if special_key == "favorite" {
                asset.favorite = true;
            } else {
                asset.ungrouped = true;
            }
        }
    }
    Ok(())
}

fn special_node_order(node: &JumpServerNode) -> u8 {
    if node.is_favorite() {
        0
    } else if node.is_ungrouped() {
        1
    } else {
        2
    }
}

async fn list_assets(client: &Client, session: &JumpServerSession) -> Result<Vec<JumpServerAsset>> {
    let organizations: Vec<Option<JumpServerOrganization>> = if session.organizations.is_empty() {
        vec![None]
    } else {
        session.organizations.iter().cloned().map(Some).collect()
    };
    let mut assets = Vec::new();
    let mut seen = HashSet::new();
    for organization in organizations {
        let org_id = organization.as_ref().map(|org| org.id.as_str());
        list_organization_assets(
            client,
            session,
            org_id,
            "api/v1/perms/users/self/assets/",
            &mut assets,
            &mut seen,
        )
        .await?;
    }
    assets.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.address.cmp(&right.address))
    });
    Ok(assets)
}

async fn list_organization_assets(
    client: &Client,
    session: &JumpServerSession,
    org_id: Option<&str>,
    path: &str,
    assets: &mut Vec<JumpServerAsset>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let mut offset = 0usize;
    let mut pages = 0usize;
    loop {
        let mut url = api_url(&session.base_url, path)?;
        url.query_pairs_mut()
            .append_pair("limit", &ASSET_PAGE_SIZE.to_string())
            .append_pair("offset", &offset.to_string());
        let response = authorized_request(client.get(url), session, org_id)?
            .send()
            .await
            .map_err(|error| request_error("获取 JumpServer 资源", error))?;
        let body = response_body(response, "获取 JumpServer 资源").await?;
        let (wire_assets, count, paged) = parse_asset_page(&body)?;
        if count.is_some_and(|total| total > MAX_JUMPSERVER_ASSETS) {
            return Err(DomainError::Other(format!(
                "JumpServer 资源超过 {MAX_JUMPSERVER_ASSETS} 条上限，请缩小授权范围"
            )));
        }
        let page_len = wire_assets.len();
        let seen_before = seen.len();
        for wire in wire_assets {
            let asset = wire.into_asset(org_id.unwrap_or_default())?;
            if seen.insert(asset.id.clone()) {
                if assets.len() >= MAX_JUMPSERVER_ASSETS {
                    return Err(DomainError::Other(format!(
                        "JumpServer 资源超过 {MAX_JUMPSERVER_ASSETS} 条上限，请缩小授权范围"
                    )));
                }
                assets.push(asset);
            }
        }
        offset = offset.saturating_add(page_len);
        pages = pages.saturating_add(1);
        if page_len == 0 || !paged || count.is_some_and(|total| offset >= total) {
            break;
        }
        if seen.len() == seen_before || pages >= MAX_ASSET_PAGES {
            return Err(DomainError::ConnectionFailed(
                "JumpServer 资源分页异常，请稍后重试".into(),
            ));
        }
    }
    Ok(())
}

async fn asset_detail(
    client: &Client,
    session: &JumpServerSession,
    selected: &JumpServerAsset,
) -> Result<JumpServerAssetDetail> {
    selected.validate_id().map_err(DomainError::InvalidConfig)?;
    let path = format!("api/v1/perms/users/self/assets/{}/", selected.id);
    let url = api_url(&session.base_url, &path)?;
    let response = authorized_request(client.get(url), session, Some(&selected.org_id))?
        .send()
        .await
        .map_err(|error| request_error("获取资产连接信息", error))?;
    let body = response_body(response, "获取资产连接信息").await?;
    let wire: WireAssetDetail = parse_json(&body, "获取资产连接信息")?;
    let detail = wire.into_detail(&selected.org_id)?;
    if detail.asset.id != selected.id {
        return Err(DomainError::ConnectionFailed(
            "JumpServer 返回了不匹配的资产".into(),
        ));
    }
    Ok(detail)
}

async fn create_rdp_web_session(
    client: &Client,
    session: &JumpServerSession,
    asset: &JumpServerAsset,
    account: &JumpServerAccount,
) -> Result<String> {
    asset.validate_id().map_err(DomainError::InvalidConfig)?;
    account
        .validate_for_web_session()
        .map_err(DomainError::InvalidConfig)?;

    let url = api_url(&session.base_url, "api/v1/authentication/connection-token/")?;
    let response = authorized_request(client.post(url), session, Some(&asset.org_id))?
        .json(&serde_json::json!({
            "asset": asset.id,
            "account": account.alias,
            "protocol": "rdp",
            "input_username": account.username,
            "input_secret": "",
            "input_secret_type": "password",
            "connect_method": "web_gui",
            "connect_options": {},
        }))
        .send()
        .await
        .map_err(|error| request_error("创建 RDP Web 会话", error))?;
    let body = response_body(response, "创建 RDP Web 会话").await?;
    let token: WireConnectionToken = parse_json(&body, "创建 RDP Web 会话")?;
    uuid::Uuid::parse_str(&token.id)
        .map_err(|_| DomainError::ConnectionFailed("JumpServer 返回的连接令牌无效".into()))?;

    let mut endpoint_url = api_url(&session.base_url, "api/v1/terminal/endpoints/smart/")?;
    let scheme = endpoint_url.scheme().to_string();
    endpoint_url
        .query_pairs_mut()
        .append_pair("protocol", &scheme)
        .append_pair("token", &token.id);
    let response = authorized_request(client.get(endpoint_url), session, Some(&asset.org_id))?
        .send()
        .await
        .map_err(|error| request_error("获取 RDP Web 会话端点", error))?;
    let body = response_body(response, "获取 RDP Web 会话端点").await?;
    let endpoint: WireWebEndpoint = parse_json(&body, "获取 RDP Web 会话端点")?;
    build_rdp_web_session_url(&session.base_url, &endpoint, &token.id, &asset.name)
}

fn authorized_request(
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

async fn response_body(mut response: Response, operation: &str) -> Result<Vec<u8>> {
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

fn request_error(operation: &str, error: reqwest::Error) -> DomainError {
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

fn classify_connection_error(details: &str) -> &'static str {
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

fn api_error_message(value: &Value) -> Option<String> {
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

fn parse_json<T: DeserializeOwned>(bytes: &[u8], operation: &str) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|error| DomainError::ConnectionFailed(format!("{operation}返回格式无效：{error}")))
}

#[derive(Deserialize)]
struct WireAsset {
    id: String,
    #[serde(default)]
    org_id: String,
    name: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    platform: Value,
    #[serde(default)]
    labels: Option<Vec<WireLabel>>,
    #[serde(default)]
    nodes: Option<Vec<WireReference>>,
    #[serde(default = "default_true")]
    is_active: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireReference {
    Id(String),
    Object { id: String },
}

impl WireReference {
    fn into_id(self) -> String {
        match self {
            Self::Id(id) | Self::Object { id } => id,
        }
    }
}

#[derive(Deserialize)]
struct WireLabel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: String,
}

impl WireLabel {
    fn into_label(self) -> Result<JumpServerLabel> {
        Ok(JumpServerLabel {
            name: checked_optional_field(self.name)?,
            value: checked_optional_field(self.value)?,
        })
    }
}

impl WireAsset {
    fn into_asset(self, fallback_org_id: &str) -> Result<JumpServerAsset> {
        let mut node_ids = self
            .nodes
            .unwrap_or_default()
            .into_iter()
            .map(WireReference::into_id)
            .map(|id| checked_field("资产节点 ID", id))
            .collect::<Result<Vec<_>>>()?;
        node_ids.sort();
        node_ids.dedup();
        let asset = JumpServerAsset {
            id: checked_field("资产 ID", self.id)?,
            org_id: checked_optional_field(if self.org_id.is_empty() {
                fallback_org_id.to_string()
            } else {
                self.org_id
            })?,
            name: checked_field("资产名称", self.name)?,
            address: checked_optional_field(self.address)?,
            platform: checked_optional_field(value_label(&self.platform))?,
            labels: self
                .labels
                .unwrap_or_default()
                .into_iter()
                .map(WireLabel::into_label)
                .collect::<Result<Vec<_>>>()?,
            node_ids,
            favorite: false,
            ungrouped: false,
            active: self.is_active,
        };
        asset
            .validate_id()
            .map_err(|_| DomainError::ConnectionFailed("JumpServer 返回的资产 ID 无效".into()))?;
        Ok(asset)
    }
}

#[derive(Deserialize)]
struct WireNode {
    id: String,
    #[serde(default)]
    org_id: String,
    key: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    full_value: String,
    #[serde(default)]
    assets_amount: usize,
}

impl WireNode {
    fn into_node(self, fallback_org_id: &str) -> Result<JumpServerNode> {
        let name = if self.name.trim().is_empty() {
            self.value
        } else {
            self.name
        };
        let full_name = if self.full_value.trim().is_empty() {
            name.clone()
        } else {
            self.full_value
        };
        Ok(JumpServerNode {
            id: checked_field("资产节点 ID", self.id)?,
            org_id: checked_optional_field(if self.org_id.is_empty() {
                fallback_org_id.to_string()
            } else {
                self.org_id
            })?,
            key: checked_field("资产节点 key", self.key)?,
            name: checked_field("资产节点名称", name)?,
            full_name: checked_field("资产节点完整名称", full_name)?,
            assets_amount: self.assets_amount,
        })
    }
}

#[derive(Deserialize)]
struct WireAssetDetail {
    #[serde(flatten)]
    asset: WireAsset,
    #[serde(default)]
    permed_protocols: Vec<Value>,
    #[serde(default)]
    permed_accounts: Vec<WireAccount>,
}

impl WireAssetDetail {
    fn into_detail(self, fallback_org_id: &str) -> Result<JumpServerAssetDetail> {
        let ssh_enabled = self
            .permed_protocols
            .iter()
            .any(|protocol| protocol_is(protocol, "ssh"));
        let rdp_web_enabled = self.permed_protocols.iter().any(|protocol| {
            protocol_is(protocol, "rdp")
                && protocol
                    .get("public")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
        });
        let accounts = self
            .permed_accounts
            .into_iter()
            .map(WireAccount::into_account)
            .collect::<Result<Vec<_>>>()?;
        Ok(JumpServerAssetDetail {
            asset: self.asset.into_asset(fallback_org_id)?,
            accounts,
            ssh_enabled,
            rdp_web_enabled,
        })
    }
}

fn protocol_is(protocol: &Value, expected: &str) -> bool {
    protocol
        .as_str()
        .or_else(|| protocol.get("name").and_then(Value::as_str))
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

#[derive(Deserialize)]
struct WireAccount {
    id: String,
    #[serde(default)]
    alias: String,
    name: String,
    #[serde(default)]
    username: String,
    has_secret: Option<bool>,
    #[serde(default)]
    actions: Value,
}

impl WireAccount {
    fn into_account(self) -> Result<JumpServerAccount> {
        let id = checked_field("账号 ID", self.id)?;
        let alias = checked_optional_field(self.alias)?;
        let can_connect = self.actions.is_null()
            || self.actions.as_array().is_some_and(|actions| {
                actions.iter().any(|action| {
                    action
                        .as_str()
                        .or_else(|| action.get("value").and_then(Value::as_str))
                        .is_some_and(|name| name.eq_ignore_ascii_case("connect"))
                })
            });
        Ok(JumpServerAccount {
            alias: if alias.is_empty() { id.clone() } else { alias },
            id,
            name: checked_field("账号名称", self.name)?,
            username: checked_optional_field(self.username)?,
            has_secret: self.has_secret.unwrap_or(true),
            can_connect,
        })
    }
}

#[derive(Deserialize)]
struct WireConnectionToken {
    id: String,
}

#[derive(Deserialize)]
struct WireWebEndpoint {
    #[serde(default)]
    host: String,
    #[serde(default)]
    https_port: u16,
    #[serde(default)]
    http_port: u16,
}

fn build_rdp_web_session_url(
    base_url: &str,
    endpoint: &WireWebEndpoint,
    token_id: &str,
    asset_name: &str,
) -> Result<String> {
    uuid::Uuid::parse_str(token_id)
        .map_err(|_| DomainError::ConnectionFailed("JumpServer 返回的连接令牌无效".into()))?;
    let base = Url::parse(base_url)
        .map_err(|error| DomainError::InvalidConfig(format!("JumpServer 地址无效：{error}")))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(DomainError::InvalidConfig(
            "JumpServer 地址只支持 http 或 https".into(),
        ));
    }

    let host = checked_optional_field(endpoint.host.clone())?;
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    if !host.is_empty() {
        url.set_host(Some(&host)).map_err(|_| {
            DomainError::ConnectionFailed("JumpServer 返回的 Web 会话主机无效".into())
        })?;
    }
    let port = if url.scheme() == "https" {
        endpoint.https_port
    } else {
        endpoint.http_port
    };
    if port != 0 {
        url.set_port(Some(port)).map_err(|_| {
            DomainError::ConnectionFailed("JumpServer 返回的 Web 会话端口无效".into())
        })?;
    }

    let same_origin = url.scheme() == base.scheme()
        && url.host_str() == base.host_str()
        && url.port_or_known_default() == base.port_or_known_default();
    let path = if same_origin {
        format!("{}/lion/connect", base.path().trim_end_matches('/'))
    } else {
        "/lion/connect".into()
    };
    url.set_path(&path);
    url.query_pairs_mut()
        .append_pair("ramag_asset", asset_name)
        .append_pair("token", token_id);
    Ok(url.to_string())
}

fn parse_asset_page(bytes: &[u8]) -> Result<(Vec<WireAsset>, Option<usize>, bool)> {
    let value: Value = parse_json(bytes, "获取 JumpServer 资源")?;
    match value {
        Value::Array(items) => Ok((parse_asset_items(items)?, None, false)),
        Value::Object(mut object) => {
            let count = object
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok());
            let items = object
                .remove("results")
                .or_else(|| object.remove("data"))
                .and_then(|items| items.as_array().cloned())
                .ok_or_else(|| {
                    DomainError::ConnectionFailed("JumpServer 资源列表格式无效".into())
                })?;
            Ok((parse_asset_items(items)?, count, true))
        }
        _ => Err(DomainError::ConnectionFailed(
            "JumpServer 资源列表格式无效".into(),
        )),
    }
}

fn parse_asset_items(items: Vec<Value>) -> Result<Vec<WireAsset>> {
    items
        .into_iter()
        .map(|item| {
            serde_json::from_value(item).map_err(|error| {
                DomainError::ConnectionFailed(format!("JumpServer 资源字段无效：{error}"))
            })
        })
        .collect()
}

fn parse_node_page(bytes: &[u8]) -> Result<(Vec<WireNode>, Option<usize>, bool)> {
    let value: Value = parse_json(bytes, "获取 JumpServer 资产树")?;
    match value {
        Value::Array(items) => Ok((parse_node_items(items)?, None, false)),
        Value::Object(mut object) => {
            let count = object
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok());
            let items = object
                .remove("results")
                .or_else(|| object.remove("data"))
                .and_then(|items| items.as_array().cloned())
                .ok_or_else(|| DomainError::ConnectionFailed("JumpServer 资产树格式无效".into()))?;
            Ok((parse_node_items(items)?, count, true))
        }
        _ => Err(DomainError::ConnectionFailed(
            "JumpServer 资产树格式无效".into(),
        )),
    }
}

fn parse_node_items(items: Vec<Value>) -> Result<Vec<WireNode>> {
    items
        .into_iter()
        .map(|item| {
            serde_json::from_value(item).map_err(|error| {
                DomainError::ConnectionFailed(format!("JumpServer 资产节点字段无效：{error}"))
            })
        })
        .collect()
}

fn checked_field(label: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(DomainError::ConnectionFailed(format!(
            "JumpServer 返回的{label}为空"
        )));
    }
    checked_optional_field(value)
}

fn checked_optional_field(value: String) -> Result<String> {
    if value.len() > MAX_API_FIELD_BYTES || value.contains(['\r', '\n']) {
        return Err(DomainError::ConnectionFailed(
            "JumpServer 返回了过长或包含换行的字段".into(),
        ));
    }
    Ok(value)
}

fn value_label(value: &Value) -> String {
    value
        .as_str()
        .or_else(|| value.get("name").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_defaults_to_https_and_preserves_subpath() {
        let endpoint = normalize_endpoint("jump.example.com/console").unwrap();
        assert_eq!(endpoint.base_url, "https://jump.example.com/console/");
        assert_eq!(endpoint.ssh_host, "jump.example.com");
        assert_eq!(
            api_url(&endpoint.base_url, "api/v1/test/")
                .unwrap()
                .as_str(),
            "https://jump.example.com/console/api/v1/test/"
        );
    }

    #[test]
    fn endpoint_rejects_embedded_credentials_and_non_http_scheme() {
        assert!(normalize_endpoint("https://alice:secret@jump.example.com").is_err());
        assert!(normalize_endpoint("file:///tmp/jumpserver").is_err());
    }

    #[test]
    fn parses_paginated_and_legacy_asset_lists() {
        let page = br##"{"count":1,"results":[{"id":"asset-1","name":"login","address":"10.0.0.1","platform":{"name":"Linux"},"nodes":["node-1",{"id":"node-2"}],"labels":[{"id":"label-1","name":"env","value":"prod","color":"#ff0000"}]}]}"##;
        let (items, count, paged) = parse_asset_page(page).unwrap();
        assert_eq!(items.len(), 1);
        let labels = items[0].labels.as_ref().unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].name, "env");
        assert_eq!(labels[0].value, "prod");
        assert_eq!(items[0].nodes.as_ref().unwrap().len(), 2);
        assert_eq!(count, Some(1));
        assert!(paged);

        let legacy = br#"[{"id":"asset-1","name":"login"}]"#;
        let (items, count, paged) = parse_asset_page(legacy).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(count, None);
        assert!(!paged);
    }

    #[test]
    fn parses_asset_tree_nodes_and_special_parent_keys() {
        let page = br#"{"count":2,"results":[
            {"id":"favorite","key":"favorite","value":"Favorite","assets_amount":3},
            {"id":"node-1","org_id":"org-1","key":"1:2","name":"Industrial","full_value":"DEFAULT / Industrial","assets_amount":4}
        ]}"#;
        let (items, count, paged) = parse_node_page(page).unwrap();
        assert_eq!(count, Some(2));
        assert!(paged);

        let favorite = items
            .into_iter()
            .next()
            .unwrap()
            .into_node("org-1")
            .unwrap();
        assert!(favorite.is_favorite());
        assert_eq!(favorite.org_id, "org-1");
        assert_eq!(favorite.parent_key(), "");

        let child: WireNode = serde_json::from_str(
            r#"{"id":"node-1","key":"1:2","name":"Industrial","assets_amount":4}"#,
        )
        .unwrap();
        assert_eq!(child.into_node("org-1").unwrap().parent_key(), "1");
    }

    #[test]
    fn detail_maps_ssh_and_public_rdp_protocols_with_account_alias() {
        let wire: WireAssetDetail = serde_json::from_str(
            r##"{
                "id":"00000000-0000-0000-0000-000000000001","org_id":"org-1","name":"login","address":"10.0.0.1",
                "labels":[{"id":"label-1","name":"env","value":"prod","color":"#ff0000"}],
                "permed_protocols":[{"name":"ssh","port":22},{"name":"rdp","port":3389,"public":true}],
                "permed_accounts":[{"id":"account-1","alias":"account-1","name":"root","username":"root","has_secret":true,"actions":[{"value":"connect","label":"Connect"}]}]
            }"##,
        )
        .unwrap();
        let detail = wire.into_detail("org-1").unwrap();
        assert!(detail.ssh_enabled);
        assert!(detail.rdp_web_enabled);
        assert!(detail.accounts[0].usable_for_direct_login());
        assert_eq!(detail.accounts[0].alias, "account-1");
        assert_eq!(detail.asset.labels[0].display_name(), "env:prod");

        let legacy_account = WireAccount {
            id: "account-2".into(),
            alias: String::new(),
            name: "admin".into(),
            username: "admin".into(),
            has_secret: Some(true),
            actions: serde_json::json!(["connect"]),
        }
        .into_account()
        .unwrap();
        assert!(legacy_account.can_connect);
        assert_eq!(legacy_account.alias, "account-2");
    }

    #[test]
    fn detail_does_not_offer_private_rdp_as_a_web_session() {
        let wire: WireAssetDetail = serde_json::from_str(
            r#"{
                "id":"00000000-0000-0000-0000-000000000001","name":"windows","address":"10.0.0.2",
                "permed_protocols":[{"name":"rdp","port":3389,"public":false}]
            }"#,
        )
        .unwrap();

        let detail = wire.into_detail("org-1").unwrap();
        assert!(!detail.rdp_web_enabled);
    }

    #[test]
    fn rdp_web_session_url_uses_smart_endpoint_without_losing_site_prefix() {
        let endpoint = WireWebEndpoint {
            host: "jump.example.com".into(),
            https_port: 0,
            http_port: 0,
        };

        let url = build_rdp_web_session_url(
            "https://jump.example.com/console/",
            &endpoint,
            "00000000-0000-0000-0000-000000000002",
            "CAE365BE",
        )
        .unwrap();

        assert_eq!(
            url,
            "https://jump.example.com/console/lion/connect?ramag_asset=CAE365BE&token=00000000-0000-0000-0000-000000000002"
        );
    }

    #[test]
    fn rdp_web_session_url_encodes_asset_name_as_a_visible_label() {
        let endpoint = WireWebEndpoint {
            host: String::new(),
            https_port: 0,
            http_port: 0,
        };

        let url = build_rdp_web_session_url(
            "https://jump.example.com/",
            &endpoint,
            "00000000-0000-0000-0000-000000000002",
            "CAE 365/北京",
        )
        .unwrap();

        assert_eq!(
            url,
            "https://jump.example.com/lion/connect?ramag_asset=CAE+365%2F%E5%8C%97%E4%BA%AC&token=00000000-0000-0000-0000-000000000002"
        );
    }

    #[test]
    fn asset_rejects_non_uuid_id_before_building_detail_url() {
        let wire = WireAsset {
            id: "../asset".into(),
            org_id: "org-1".into(),
            name: "login".into(),
            address: "10.0.0.1".into(),
            platform: Value::String("Linux".into()),
            labels: None,
            nodes: None,
            is_active: true,
        };

        assert!(wire.into_asset("org-1").is_err());
    }

    #[test]
    fn organizations_are_validated_and_deduplicated() {
        let organizations = validate_organizations(vec![
            LoginOrganization {
                id: "org-1".into(),
                name: "DEFAULT".into(),
            },
            LoginOrganization {
                id: "org-1".into(),
                name: "DEFAULT".into(),
            },
        ])
        .unwrap();
        assert_eq!(organizations.len(), 1);
        assert!(
            validate_organizations(vec![LoginOrganization {
                id: "bad\nheader".into(),
                name: "DEFAULT".into(),
            }])
            .is_err()
        );
    }

    #[test]
    fn connection_errors_are_classified_without_exposing_transport_details() {
        assert_eq!(
            classify_connection_error("invalid peer certificate: UnknownIssuer"),
            "TLS 证书校验失败"
        );
        assert_eq!(
            classify_connection_error("dns error: failed to lookup address"),
            "域名解析失败"
        );
        assert_eq!(
            classify_connection_error("tcp connect error: Connection refused"),
            "服务器拒绝连接"
        );
        assert_eq!(
            classify_connection_error("tcp connect error: Operation not permitted"),
            "系统阻止网络连接"
        );
        assert_eq!(
            classify_connection_error("connection reset by peer"),
            "连接被网络或服务器中断"
        );
        assert_eq!(
            classify_connection_error("failed to connect to proxy"),
            "代理服务器连接失败"
        );
        assert_eq!(
            classify_connection_error("client error with private internal details"),
            "无法连接服务器"
        );
    }
}
