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

mod catalog;
mod http;
mod wire;

use catalog::{
    asset_detail, create_rdp_web_session, fetch_workbench_organizations, load_catalog,
    validate_organizations,
};
#[cfg(test)]
use http::classify_connection_error;
use http::{api_error_message, authorized_request, parse_json, request_error, response_body};
use wire::*;

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
