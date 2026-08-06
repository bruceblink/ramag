//! JumpServer 登录会话、授权资产与账号。

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ssh::{MAX_SSH_PASSWORD_BYTES, MAX_SSH_USERNAME_BYTES};

pub const MAX_JUMPSERVER_URL_BYTES: usize = 2048;
pub const MAX_JUMPSERVER_TOKEN_BYTES: usize = 64 * 1024;
pub const MAX_JUMPSERVER_ASSETS: usize = 10_000;
pub const MAX_JUMPSERVER_NODES: usize = 10_000;
pub const MAX_JUMPSERVER_RDP_RECENT_SESSIONS: usize = 20;
pub const MAX_JUMPSERVER_RDP_FAVORITE_SESSIONS: usize = 50;
const MAX_JUMPSERVER_RDP_FIELD_BYTES: usize = 4096;

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
    /// JumpServer 创建连接令牌时使用的账号别名；普通账号通常等于账号 ID。
    pub alias: String,
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

    pub fn validate_for_web_session(&self) -> Result<(), String> {
        if !self.can_connect {
            return Err("该账号缺少连接权限".into());
        }
        if !self.has_secret {
            return Err("该账号未托管密码，无法直接打开远程会话".into());
        }
        let alias = self.alias.trim();
        if alias.is_empty() {
            return Err("资产账号别名为空".into());
        }
        if alias.len() > MAX_SSH_USERNAME_BYTES || alias.chars().any(char::is_control) {
            return Err("资产账号别名过长或包含控制字符".into());
        }
        Ok(())
    }

    pub fn usable_for_web_session(&self) -> bool {
        self.validate_for_web_session().is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpServerAssetDetail {
    pub asset: JumpServerAsset,
    pub accounts: Vec<JumpServerAccount>,
    pub ssh_enabled: bool,
    /// 仅当当前授权包含可公开使用的 RDP 协议时启用浏览器会话入口。
    pub rdp_web_enabled: bool,
}

/// 可持久化的 RDP 目标定位信息；不包含登录密码、API Token 或连接 Token。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpServerRdpSession {
    pub connection_id: String,
    pub jumpserver_url: String,
    pub asset_id: String,
    pub org_id: String,
    pub asset_name: String,
    pub asset_address: String,
    pub asset_platform: String,
    pub account_id: String,
    pub account_name: String,
    pub account_username: String,
}

impl JumpServerRdpSession {
    pub fn from_selection(
        connection_id: String,
        jumpserver_url: String,
        asset: &JumpServerAsset,
        account: &JumpServerAccount,
    ) -> Result<Self, String> {
        let session = Self {
            connection_id,
            jumpserver_url,
            asset_id: asset.id.clone(),
            org_id: asset.org_id.clone(),
            asset_name: asset.name.clone(),
            asset_address: asset.address.clone(),
            asset_platform: asset.platform.clone(),
            account_id: account.id.clone(),
            account_name: account.name.clone(),
            account_username: account.username.clone(),
        };
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), String> {
        Uuid::parse_str(&self.connection_id).map_err(|_| "JumpServer 连接 ID 不是有效的 UUID")?;
        Uuid::parse_str(&self.asset_id).map_err(|_| "资产 ID 不是有效的 UUID")?;
        validate_rdp_field(
            "JumpServer 地址",
            &self.jumpserver_url,
            MAX_JUMPSERVER_URL_BYTES,
            true,
        )?;
        if self.jumpserver_url.chars().any(char::is_whitespace) {
            return Err("JumpServer 地址不能包含空白字符".into());
        }
        validate_rdp_field(
            "组织 ID",
            &self.org_id,
            MAX_JUMPSERVER_RDP_FIELD_BYTES,
            false,
        )?;
        validate_rdp_field(
            "资产名称",
            &self.asset_name,
            MAX_JUMPSERVER_RDP_FIELD_BYTES,
            true,
        )?;
        validate_rdp_field(
            "资产地址",
            &self.asset_address,
            MAX_JUMPSERVER_RDP_FIELD_BYTES,
            false,
        )?;
        validate_rdp_field(
            "资产平台",
            &self.asset_platform,
            MAX_JUMPSERVER_RDP_FIELD_BYTES,
            false,
        )?;
        validate_rdp_field(
            "账号 ID",
            &self.account_id,
            MAX_JUMPSERVER_RDP_FIELD_BYTES,
            true,
        )?;
        validate_rdp_field(
            "账号名称",
            &self.account_name,
            MAX_JUMPSERVER_RDP_FIELD_BYTES,
            true,
        )?;
        validate_rdp_field(
            "账号用户名",
            &self.account_username,
            MAX_JUMPSERVER_RDP_FIELD_BYTES,
            false,
        )
    }

    pub fn same_target(&self, other: &Self) -> bool {
        self.connection_id == other.connection_id
            && self.asset_id == other.asset_id
            && self.account_id == other.account_id
    }

    pub fn asset_snapshot(&self) -> JumpServerAsset {
        JumpServerAsset {
            id: self.asset_id.clone(),
            org_id: self.org_id.clone(),
            name: self.asset_name.clone(),
            address: self.asset_address.clone(),
            platform: self.asset_platform.clone(),
            labels: Vec::new(),
            node_ids: Vec::new(),
            favorite: false,
            ungrouped: false,
            active: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct JumpServerRdpSessionHistory {
    #[serde(default)]
    pub favorites: Vec<JumpServerRdpSession>,
    #[serde(default)]
    pub recent: Vec<JumpServerRdpSession>,
}

impl JumpServerRdpSessionHistory {
    pub fn validate(&self) -> Result<(), String> {
        if self.favorites.len() > MAX_JUMPSERVER_RDP_FAVORITE_SESSIONS {
            return Err(format!(
                "收藏远程会话不能超过 {MAX_JUMPSERVER_RDP_FAVORITE_SESSIONS} 个"
            ));
        }
        if self.recent.len() > MAX_JUMPSERVER_RDP_RECENT_SESSIONS {
            return Err(format!(
                "最近远程会话不能超过 {MAX_JUMPSERVER_RDP_RECENT_SESSIONS} 个"
            ));
        }
        let mut identities = std::collections::HashSet::new();
        for session in self.favorites.iter().chain(&self.recent) {
            session.validate()?;
            let identity = (
                session.connection_id.as_str(),
                session.asset_id.as_str(),
                session.account_id.as_str(),
            );
            if !identities.insert(identity) {
                return Err("远程会话记录重复".into());
            }
        }
        Ok(())
    }

    pub fn record_open(&mut self, session: JumpServerRdpSession) -> Result<(), String> {
        session.validate()?;
        let favorite = self
            .favorites
            .iter()
            .any(|existing| existing.same_target(&session));
        self.remove_target(&session);
        if favorite {
            self.favorites.insert(0, session);
            self.sort_favorites_by_name();
        } else {
            self.recent.insert(0, session);
            self.recent.truncate(MAX_JUMPSERVER_RDP_RECENT_SESSIONS);
        }
        self.validate()
    }

    pub fn set_favorite(
        &mut self,
        session: &JumpServerRdpSession,
        favorite: bool,
    ) -> Result<(), String> {
        session.validate()?;
        let was_favorite = self
            .favorites
            .iter()
            .any(|existing| existing.same_target(session));
        if favorite && !was_favorite && self.favorites.len() >= MAX_JUMPSERVER_RDP_FAVORITE_SESSIONS
        {
            return Err(format!(
                "收藏远程会话最多 {MAX_JUMPSERVER_RDP_FAVORITE_SESSIONS} 个"
            ));
        }
        self.remove_target(session);
        if favorite {
            self.favorites.insert(0, session.clone());
            self.sort_favorites_by_name();
        } else {
            self.recent.insert(0, session.clone());
            self.recent.truncate(MAX_JUMPSERVER_RDP_RECENT_SESSIONS);
        }
        self.validate()
    }

    /// 收藏按资源名称展示；同名资源使用地址和账号保证顺序稳定。
    pub fn sort_favorites_by_name(&mut self) {
        self.favorites.sort_by(|left, right| {
            left.asset_name
                .to_lowercase()
                .cmp(&right.asset_name.to_lowercase())
                .then_with(|| left.asset_name.cmp(&right.asset_name))
                .then_with(|| left.asset_address.cmp(&right.asset_address))
                .then_with(|| left.account_username.cmp(&right.account_username))
                .then_with(|| left.connection_id.cmp(&right.connection_id))
        });
    }

    fn remove_target(&mut self, session: &JumpServerRdpSession) {
        self.favorites
            .retain(|existing| !existing.same_target(session));
        self.recent
            .retain(|existing| !existing.same_target(session));
    }
}

fn validate_rdp_field(
    label: &str,
    value: &str,
    max_bytes: usize,
    required: bool,
) -> Result<(), String> {
    if required && value.trim().is_empty() {
        return Err(format!("{label}不能为空"));
    }
    if value.len() > max_bytes {
        return Err(format!("{label}不能超过 {max_bytes} 字节"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
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
            alias: "account-1".into(),
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
            alias: "account-1".into(),
            name: "root".into(),
            username: "root".into(),
            has_secret: false,
            can_connect: true,
        };

        assert!(account.usable_for_direct_login());
        account.can_connect = false;
        assert!(!account.usable_for_direct_login());
    }

    #[test]
    fn web_session_requires_managed_secret_and_connect_permission() {
        let mut account = JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "Administrator".into(),
            username: "Administrator".into(),
            has_secret: true,
            can_connect: true,
        };

        assert!(account.validate_for_web_session().is_ok());
        account.has_secret = false;
        assert!(account.validate_for_web_session().is_err());
        account.has_secret = true;
        account.can_connect = false;
        assert!(account.validate_for_web_session().is_err());
    }

    fn rdp_session(index: u32) -> JumpServerRdpSession {
        JumpServerRdpSession {
            connection_id: "00000000-0000-0000-0000-000000000010".into(),
            jumpserver_url: "https://jump.example.com".into(),
            asset_id: format!("00000000-0000-0000-0000-{index:012}"),
            org_id: "org-1".into(),
            asset_name: format!("windows-{index}"),
            asset_address: format!("10.0.0.{index}"),
            asset_platform: "Windows".into(),
            account_id: "account-1".into(),
            account_name: "admin".into(),
            account_username: "Administrator".into(),
        }
    }

    #[test]
    fn rdp_history_moves_sessions_between_recent_and_favorites_without_duplicates() {
        let first = rdp_session(1);
        let second = rdp_session(2);
        let mut history = JumpServerRdpSessionHistory::default();

        assert_eq!(history.record_open(first.clone()), Ok(()));
        assert_eq!(history.record_open(second.clone()), Ok(()));
        assert_eq!(history.recent, vec![second.clone(), first.clone()]);

        assert_eq!(history.set_favorite(&first, true), Ok(()));
        assert_eq!(history.favorites, vec![first.clone()]);
        assert_eq!(history.recent, vec![second.clone()]);

        let mut renamed = first.clone();
        renamed.asset_name = "windows-renamed".into();
        assert_eq!(history.record_open(renamed.clone()), Ok(()));
        assert_eq!(history.favorites, vec![renamed.clone()]);
        assert_eq!(history.recent, vec![second.clone()]);

        assert_eq!(history.set_favorite(&renamed, false), Ok(()));
        assert!(history.favorites.is_empty());
        assert_eq!(history.recent, vec![renamed, second]);
        assert_eq!(history.validate(), Ok(()));
    }

    #[test]
    fn rdp_history_bounds_recent_sessions_and_rejects_invalid_targets() {
        let mut history = JumpServerRdpSessionHistory::default();
        for index in 1..=(MAX_JUMPSERVER_RDP_RECENT_SESSIONS as u32 + 1) {
            assert_eq!(history.record_open(rdp_session(index)), Ok(()));
        }
        assert_eq!(history.recent.len(), MAX_JUMPSERVER_RDP_RECENT_SESSIONS);
        assert_eq!(history.recent[0].asset_name, "windows-21");

        let mut invalid = rdp_session(1);
        invalid.connection_id = "not-a-uuid".into();
        assert!(invalid.validate().is_err());
        assert!(history.record_open(invalid).is_err());
    }

    #[test]
    fn rdp_favorites_are_sorted_by_asset_name() {
        let mut alpha = rdp_session(1);
        alpha.asset_name = "Alpha".into();
        let mut beta = rdp_session(2);
        beta.asset_name = "beta".into();
        let mut zeta = rdp_session(3);
        zeta.asset_name = "zeta".into();
        let mut history = JumpServerRdpSessionHistory::default();

        assert_eq!(history.set_favorite(&zeta, true), Ok(()));
        assert_eq!(history.set_favorite(&beta, true), Ok(()));
        assert_eq!(history.set_favorite(&alpha, true), Ok(()));

        assert_eq!(history.favorites, vec![alpha, beta, zeta]);
    }
}
