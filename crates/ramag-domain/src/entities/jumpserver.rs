//! JumpServer 登录会话、授权资产与账号。

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ssh::{MAX_SSH_PASSWORD_BYTES, MAX_SSH_USERNAME_BYTES};

pub const MAX_JUMPSERVER_URL_BYTES: usize = 2048;
pub const MAX_JUMPSERVER_TOKEN_BYTES: usize = 64 * 1024;
pub const MAX_JUMPSERVER_ASSETS: usize = 10_000;
pub const MAX_JUMPSERVER_NODES: usize = 10_000;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpServerCredential {
    pub base_url: String,
    pub ssh_port: u16,
    pub username: String,
    pub password: String,
}

impl JumpServerCredential {
    pub fn validate(&self) -> Result<(), String> {
        let url = self.base_url.trim();
        if url.is_empty() {
            return Err("请填写 JumpServer 地址".into());
        }
        if url.len() > MAX_JUMPSERVER_URL_BYTES {
            return Err(format!(
                "JumpServer 地址不能超过 {MAX_JUMPSERVER_URL_BYTES} 字节"
            ));
        }
        if url.chars().any(char::is_whitespace) {
            return Err("JumpServer 地址不能包含空白字符".into());
        }
        if self.ssh_port == 0 {
            return Err("SSH 端口必须是 1 - 65535".into());
        }
        let username = self.username.trim();
        if username.is_empty() {
            return Err("请填写 JumpServer 用户名".into());
        }
        if username.len() > MAX_SSH_USERNAME_BYTES {
            return Err(format!(
                "JumpServer 用户名不能超过 {MAX_SSH_USERNAME_BYTES} 字节"
            ));
        }
        if username
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '#' | '@'))
        {
            return Err("JumpServer 用户名不能包含空白、# 或 @".into());
        }
        if self.password.is_empty() {
            return Err("请填写 JumpServer 登录密码".into());
        }
        if self.password.len() > MAX_SSH_PASSWORD_BYTES {
            return Err(format!(
                "JumpServer 登录密码不能超过 {MAX_SSH_PASSWORD_BYTES} 字节"
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for JumpServerCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JumpServerCredential")
            .field("base_url", &self.base_url)
            .field("ssh_port", &self.ssh_port)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpServerConnection {
    pub id: String,
    pub credential: JumpServerCredential,
}

impl JumpServerConnection {
    pub fn new(credential: JumpServerCredential) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            credential,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        Uuid::parse_str(&self.id).map_err(|_| "JumpServer 连接 ID 不是有效的 UUID")?;
        self.credential.validate()
    }
}

impl fmt::Debug for JumpServerConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JumpServerConnection")
            .field("id", &self.id)
            .field("credential", &self.credential)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerOrganization {
    pub id: String,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct JumpServerSession {
    pub base_url: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub username: String,
    pub password: String,
    pub token_keyword: String,
    pub token: String,
    pub organizations: Vec<JumpServerOrganization>,
}

impl fmt::Debug for JumpServerSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JumpServerSession")
            .field("base_url", &self.base_url)
            .field("ssh_host", &self.ssh_host)
            .field("ssh_port", &self.ssh_port)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("token_keyword", &self.token_keyword)
            .field("token", &"[REDACTED]")
            .field("organizations", &self.organizations)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerLabel {
    pub name: String,
    pub value: String,
}

impl JumpServerLabel {
    pub fn display_name(&self) -> String {
        match (self.name.trim(), self.value.trim()) {
            ("", value) => value.to_string(),
            (name, "") => name.to_string(),
            (name, value) => format!("{name}:{value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerAsset {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub address: String,
    pub platform: String,
    pub labels: Vec<JumpServerLabel>,
    pub node_ids: Vec<String>,
    pub favorite: bool,
    pub ungrouped: bool,
    pub active: bool,
}

impl JumpServerAsset {
    pub fn validate_id(&self) -> Result<(), String> {
        Uuid::parse_str(&self.id)
            .map(|_| ())
            .map_err(|_| "资产 ID 不是有效的 UUID".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerNode {
    pub id: String,
    pub org_id: String,
    pub key: String,
    pub name: String,
    pub full_name: String,
    pub assets_amount: usize,
}

impl JumpServerNode {
    pub fn is_favorite(&self) -> bool {
        self.key == "favorite"
    }

    pub fn is_ungrouped(&self) -> bool {
        self.key == "ungrouped"
    }

    pub fn is_special(&self) -> bool {
        self.is_favorite() || self.is_ungrouped()
    }

    pub fn parent_key(&self) -> &str {
        if self.is_special() {
            return "";
        }
        self.key.rsplit_once(':').map_or("", |(parent, _)| parent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerCatalog {
    pub assets: Vec<JumpServerAsset>,
    pub nodes: Vec<JumpServerNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerAccount {
    pub id: String,
    pub name: String,
    pub username: String,
    pub has_secret: bool,
    pub can_connect: bool,
}

impl JumpServerAccount {
    pub fn validate_for_direct_login(&self) -> Result<(), String> {
        if !self.can_connect {
            return Err("该账号缺少连接权限".into());
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err("资产账号名称为空".into());
        }
        if name
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '#' | '@'))
        {
            return Err("资产账号名称不能包含空白、# 或 @".into());
        }
        Ok(())
    }

    pub fn usable_for_direct_login(&self) -> bool {
        self.validate_for_direct_login().is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerAssetDetail {
    pub asset: JumpServerAsset,
    pub accounts: Vec<JumpServerAccount>,
    pub ssh_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_redacts_password_and_token() {
        let credential = JumpServerCredential {
            base_url: "https://jump.example.com".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "secret-password".into(),
        };
        let session = JumpServerSession {
            base_url: credential.base_url.clone(),
            ssh_host: "jump.example.com".into(),
            ssh_port: credential.ssh_port,
            username: credential.username.clone(),
            password: credential.password.clone(),
            token_keyword: "Bearer".into(),
            token: "secret-token".into(),
            organizations: Vec::new(),
        };

        assert!(!format!("{credential:?}").contains("secret-password"));
        let connection = JumpServerConnection::new(credential.clone());
        assert!(!format!("{connection:?}").contains("secret-password"));
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("secret-password"));
        assert!(!rendered.contains("secret-token"));
    }

    #[test]
    fn credential_rejects_direct_login_delimiters() {
        let credential = JumpServerCredential {
            base_url: "https://jump.example.com".into(),
            ssh_port: 2222,
            username: "alice#root".into(),
            password: "password".into(),
        };

        assert!(credential.validate().is_err());
    }

    #[test]
    fn direct_login_rejects_invalid_asset_and_account_identifiers() {
        let asset = JumpServerAsset {
            id: "../asset".into(),
            org_id: String::new(),
            name: "login".into(),
            address: "10.0.0.1".into(),
            platform: "Linux".into(),
            labels: Vec::new(),
            node_ids: Vec::new(),
            favorite: false,
            ungrouped: false,
            active: true,
        };
        let account = JumpServerAccount {
            id: "account-1".into(),
            name: "root#ops".into(),
            username: "root".into(),
            has_secret: true,
            can_connect: true,
        };

        assert!(asset.validate_id().is_err());
        assert!(!account.usable_for_direct_login());
    }

    #[test]
    fn direct_login_requires_connect_permission_not_managed_secret_flag() {
        let mut account = JumpServerAccount {
            id: "account-1".into(),
            name: "root".into(),
            username: "root".into(),
            has_secret: false,
            can_connect: true,
        };

        assert!(account.usable_for_direct_login());
        account.can_connect = false;
        assert!(!account.usable_for_direct_login());
    }
}
