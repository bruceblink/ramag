//! JumpServer 资源到本地 SSH 配置的用例编排。

use ramag_domain::entities::{
    JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog, JumpServerConnection,
    JumpServerCredential, JumpServerRdpSession, JumpServerRdpSessionHistory, JumpServerSession,
    SshAuthMode, SshProfile, SshProfileOrigin,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::JumpServerDriver;

use super::SshService;

const JUMPSERVER_CONNECTIONS_PREFERENCE_KEY: &str = "ssh_jumpserver_connections_v2";
const LEGACY_CREDENTIAL_PREFERENCE_KEY: &str = "ssh_jumpserver_credential_v1";
const ENCRYPTED_CREDENTIAL_PREFIX: &str = "enc-v1:";
const MAX_JUMPSERVER_CONNECTIONS: usize = 50;
const MAX_CONNECTIONS_BYTES: usize = 64 * 1024;
const MAX_ENCRYPTED_CONNECTIONS_BYTES: usize = MAX_CONNECTIONS_BYTES * 2 + 1024;
const JUMPSERVER_RDP_SESSIONS_PREFERENCE_KEY: &str = "ssh_jumpserver_rdp_sessions_v1";
const MAX_RDP_SESSIONS_BYTES: usize = 512 * 1024;
const MAX_ENCRYPTED_RDP_SESSIONS_BYTES: usize = MAX_RDP_SESSIONS_BYTES * 2 + 1024;

impl SshService {
    pub fn with_jumpserver_driver(mut self, driver: std::sync::Arc<dyn JumpServerDriver>) -> Self {
        self.jumpserver_driver = Some(driver);
        self
    }

    pub async fn authenticate_jumpserver(
        &self,
        credential: &JumpServerCredential,
    ) -> Result<JumpServerSession> {
        credential.validate().map_err(DomainError::InvalidConfig)?;
        self.jumpserver_driver()?.authenticate(credential).await
    }

    /// 读取本机加密保存的 JumpServer 连接，并自动迁移旧版单连接数据。
    pub async fn load_jumpserver_connections(&self) -> Result<Vec<JumpServerConnection>> {
        if let Some(stored) = self
            .storage
            .get_preference(JUMPSERVER_CONNECTIONS_PREFERENCE_KEY)
            .await?
        {
            let connections: Vec<JumpServerConnection> =
                self.decrypt_jumpserver_value(&stored).await?;
            let original_len = connections.len();
            let connections = deduplicate_connections(connections);
            validate_connections(&connections)?;
            if connections.len() != original_len {
                self.store_jumpserver_connections(&connections).await?;
            }
            return Ok(connections);
        }

        let Some(stored) = self
            .storage
            .get_preference(LEGACY_CREDENTIAL_PREFERENCE_KEY)
            .await?
        else {
            return Ok(Vec::new());
        };
        let credential: JumpServerCredential = self.decrypt_jumpserver_value(&stored).await?;
        credential.validate().map_err(|error| {
            DomainError::Storage(format!("已保存的 JumpServer 登录信息无效：{error}"))
        })?;
        let connections = vec![JumpServerConnection::new(credential)];
        self.store_jumpserver_connections(&connections).await?;
        self.storage
            .delete_preference(LEGACY_CREDENTIAL_PREFERENCE_KEY)
            .await?;
        Ok(connections)
    }

    /// 新建或更新连接；整份连接列表使用本机主密钥加密后保存。
    pub async fn save_jumpserver_connection(
        &self,
        connection_id: Option<&str>,
        credential: &JumpServerCredential,
    ) -> Result<JumpServerConnection> {
        credential.validate().map_err(DomainError::InvalidConfig)?;
        let mut connections = self.load_jumpserver_connections().await?;
        let connection = if let Some(connection_id) = connection_id {
            let index = connections
                .iter()
                .position(|connection| connection.id == connection_id)
                .ok_or_else(|| DomainError::NotFound("选中的 JumpServer 连接已不存在".into()))?;
            let mut connection = connections.remove(index);
            connection.credential = credential.clone();
            connection
        } else if let Some(index) = connections
            .iter()
            .position(|connection| same_connection_identity(&connection.credential, credential))
        {
            let mut connection = connections.remove(index);
            connection.credential = credential.clone();
            connection
        } else {
            if connections.len() >= MAX_JUMPSERVER_CONNECTIONS {
                return Err(DomainError::InvalidConfig(format!(
                    "JumpServer 连接最多保存 {MAX_JUMPSERVER_CONNECTIONS} 个"
                )));
            }
            JumpServerConnection::new(credential.clone())
        };
        connections
            .retain(|item| !same_connection_identity(&item.credential, &connection.credential));
        connections.insert(0, connection.clone());
        self.store_jumpserver_connections(&connections).await?;
        Ok(connection)
    }

    pub async fn delete_jumpserver_connection(&self, connection_id: &str) -> Result<()> {
        let mut connections = self.load_jumpserver_connections().await?;
        let previous_len = connections.len();
        connections.retain(|connection| connection.id != connection_id);
        if connections.len() == previous_len {
            return Err(DomainError::NotFound(
                "选中的 JumpServer 连接已不存在".into(),
            ));
        }
        if connections.is_empty() {
            self.storage
                .delete_preference(JUMPSERVER_CONNECTIONS_PREFERENCE_KEY)
                .await
        } else {
            self.store_jumpserver_connections(&connections).await
        }
    }

    pub async fn load_jumpserver_catalog(
        &self,
        session: &JumpServerSession,
    ) -> Result<JumpServerCatalog> {
        self.jumpserver_driver()?.load_catalog(session).await
    }

    pub async fn jumpserver_asset_detail(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail> {
        self.jumpserver_driver()?.asset_detail(session, asset).await
    }

    /// 重新校验授权后创建一次性 RDP Web 会话，地址只返回内存供浏览器立即打开。
    pub async fn create_jumpserver_rdp_web_session(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
        account_id: &str,
    ) -> Result<String> {
        let driver = self.jumpserver_driver()?;
        let detail = driver.asset_detail(session, asset).await?;
        if !detail.asset.active {
            return Err(DomainError::InvalidConfig("该资产已停用".into()));
        }
        if !detail.rdp_web_enabled {
            return Err(DomainError::InvalidConfig(
                "该资产未开放 RDP Web 协议".into(),
            ));
        }
        let account = detail
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .ok_or_else(|| DomainError::NotFound("选中的授权账号已失效，请重新选择".into()))?;
        account
            .validate_for_web_session()
            .map_err(DomainError::InvalidConfig)?;
        driver
            .create_rdp_web_session(session, &detail.asset, account)
            .await
    }

    /// 快捷入口只保存目标 ID；每次打开仍重新登录并校验实时授权。
    pub async fn create_saved_jumpserver_rdp_web_session(
        &self,
        saved: &JumpServerRdpSession,
    ) -> Result<String> {
        saved.validate().map_err(DomainError::InvalidConfig)?;
        let connection = self
            .load_jumpserver_connections()
            .await?
            .into_iter()
            .find(|connection| connection.id == saved.connection_id)
            .ok_or_else(|| {
                DomainError::NotFound("对应的 JumpServer 登录连接已删除，请重新选择资源".into())
            })?;
        let session = self.authenticate_jumpserver(&connection.credential).await?;
        self.create_jumpserver_rdp_web_session(&session, &saved.asset_snapshot(), &saved.account_id)
            .await
    }

    pub async fn load_jumpserver_rdp_sessions(&self) -> Result<JumpServerRdpSessionHistory> {
        let Some(stored) = self
            .storage
            .get_preference(JUMPSERVER_RDP_SESSIONS_PREFERENCE_KEY)
            .await?
        else {
            return Ok(JumpServerRdpSessionHistory::default());
        };
        if stored.len() > MAX_ENCRYPTED_RDP_SESSIONS_BYTES {
            return Err(DomainError::Storage("远程会话记录数据过大".into()));
        }
        let encoded = stored
            .strip_prefix(ENCRYPTED_CREDENTIAL_PREFIX)
            .ok_or_else(|| DomainError::Storage("远程会话记录未加密，已拒绝读取".into()))?;
        let encrypted = hex::decode(encoded)
            .map_err(|error| DomainError::Storage(format!("远程会话记录编码无效：{error}")))?;
        let plain = self.storage.unseal(&encrypted).await?;
        if plain.len() > MAX_RDP_SESSIONS_BYTES {
            return Err(DomainError::Storage("解密后的远程会话记录数据过大".into()));
        }
        let mut history: JumpServerRdpSessionHistory = serde_json::from_slice(&plain)
            .map_err(|error| DomainError::Storage(format!("解析远程会话记录失败：{error}")))?;
        history
            .validate()
            .map_err(|error| DomainError::Storage(format!("已保存的远程会话记录无效：{error}")))?;
        history.sort_favorites_by_name();
        Ok(history)
    }

    pub async fn record_jumpserver_rdp_session(
        &self,
        session: JumpServerRdpSession,
    ) -> Result<JumpServerRdpSessionHistory> {
        let mut history = self.load_jumpserver_rdp_sessions().await?;
        history
            .record_open(session)
            .map_err(DomainError::InvalidConfig)?;
        self.store_jumpserver_rdp_sessions(&history).await?;
        Ok(history)
    }

    pub async fn set_jumpserver_rdp_session_favorite(
        &self,
        session: &JumpServerRdpSession,
        favorite: bool,
    ) -> Result<JumpServerRdpSessionHistory> {
        let mut history = self.load_jumpserver_rdp_sessions().await?;
        history
            .set_favorite(session, favorite)
            .map_err(DomainError::InvalidConfig)?;
        self.store_jumpserver_rdp_sessions(&history).await?;
        Ok(history)
    }

    /// 测试前重新读取资产详情，避免账号权限变化后仍使用旧连接信息。
    pub async fn test_jumpserver_asset(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
        account_id: &str,
    ) -> Result<SshProfile> {
        let profile = self
            .fresh_jumpserver_profile(session, asset, account_id)
            .await?;
        self.test_connection(&profile).await?;
        Ok(profile)
    }

    /// 保存前重新读取资产详情；只持久化最终 SSH 配置，不保存 API 令牌。
    pub async fn save_jumpserver_asset(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
        account_id: &str,
    ) -> Result<SshProfile> {
        let profile = self
            .fresh_jumpserver_profile(session, asset, account_id)
            .await?;
        self.save_profile(&profile).await?;
        Ok(profile)
    }

    fn jumpserver_driver(&self) -> Result<&std::sync::Arc<dyn JumpServerDriver>> {
        self.jumpserver_driver.as_ref().ok_or_else(|| {
            DomainError::NotImplemented("当前构建未启用 JumpServer HTTP 客户端".into())
        })
    }

    async fn decrypt_jumpserver_value<T: serde::de::DeserializeOwned>(
        &self,
        stored: &str,
    ) -> Result<T> {
        if stored.len() > MAX_ENCRYPTED_CONNECTIONS_BYTES {
            return Err(DomainError::Storage(
                "已保存的 JumpServer 连接数据过大".into(),
            ));
        }
        let encoded = stored
            .strip_prefix(ENCRYPTED_CREDENTIAL_PREFIX)
            .ok_or_else(|| DomainError::Storage("JumpServer 连接未加密，已拒绝读取".into()))?;
        let encrypted = hex::decode(encoded)
            .map_err(|error| DomainError::Storage(format!("JumpServer 连接编码无效：{error}")))?;
        let plain = self.storage.unseal(&encrypted).await?;
        if plain.len() > MAX_CONNECTIONS_BYTES {
            return Err(DomainError::Storage(
                "解密后的 JumpServer 连接数据过大".into(),
            ));
        }
        serde_json::from_slice(&plain)
            .map_err(|error| DomainError::Storage(format!("解析 JumpServer 连接失败：{error}")))
    }

    async fn store_jumpserver_connections(
        &self,
        connections: &[JumpServerConnection],
    ) -> Result<()> {
        validate_connections(connections)?;
        let json = serde_json::to_vec(connections).map_err(|error| {
            DomainError::Storage(format!("序列化 JumpServer 连接失败：{error}"))
        })?;
        if json.len() > MAX_CONNECTIONS_BYTES {
            return Err(DomainError::InvalidConfig("JumpServer 连接数据过大".into()));
        }
        let encrypted = self.storage.seal(&json).await?;
        let stored = format!("{ENCRYPTED_CREDENTIAL_PREFIX}{}", hex::encode(encrypted));
        if stored.len() > MAX_ENCRYPTED_CONNECTIONS_BYTES {
            return Err(DomainError::InvalidConfig(
                "加密后的 JumpServer 连接数据过大".into(),
            ));
        }
        self.storage
            .set_preference(JUMPSERVER_CONNECTIONS_PREFERENCE_KEY, &stored)
            .await
    }

    async fn store_jumpserver_rdp_sessions(
        &self,
        history: &JumpServerRdpSessionHistory,
    ) -> Result<()> {
        history.validate().map_err(DomainError::InvalidConfig)?;
        let json = serde_json::to_vec(history)
            .map_err(|error| DomainError::Storage(format!("序列化远程会话记录失败：{error}")))?;
        if json.len() > MAX_RDP_SESSIONS_BYTES {
            return Err(DomainError::InvalidConfig("远程会话记录数据过大".into()));
        }
        let encrypted = self.storage.seal(&json).await?;
        let stored = format!("{ENCRYPTED_CREDENTIAL_PREFIX}{}", hex::encode(encrypted));
        if stored.len() > MAX_ENCRYPTED_RDP_SESSIONS_BYTES {
            return Err(DomainError::InvalidConfig(
                "加密后的远程会话记录数据过大".into(),
            ));
        }
        self.storage
            .set_preference(JUMPSERVER_RDP_SESSIONS_PREFERENCE_KEY, &stored)
            .await
    }

    async fn fresh_jumpserver_profile(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
        account_id: &str,
    ) -> Result<SshProfile> {
        let detail = self
            .jumpserver_driver()?
            .asset_detail(session, asset)
            .await?;
        build_jumpserver_profile(session, &detail, account_id)
    }
}

fn validate_connections(connections: &[JumpServerConnection]) -> Result<()> {
    if connections.len() > MAX_JUMPSERVER_CONNECTIONS {
        return Err(DomainError::Storage(format!(
            "已保存的 JumpServer 连接超过 {MAX_JUMPSERVER_CONNECTIONS} 个上限"
        )));
    }
    let mut ids = std::collections::HashSet::new();
    for connection in connections {
        connection.validate().map_err(|error| {
            DomainError::Storage(format!("已保存的 JumpServer 连接无效：{error}"))
        })?;
        if !ids.insert(connection.id.as_str()) {
            return Err(DomainError::Storage(
                "已保存的 JumpServer 连接 ID 重复".into(),
            ));
        }
    }
    Ok(())
}

fn deduplicate_connections(connections: Vec<JumpServerConnection>) -> Vec<JumpServerConnection> {
    let mut unique = Vec::with_capacity(connections.len());
    for connection in connections {
        if unique.iter().any(|saved: &JumpServerConnection| {
            same_connection_identity(&saved.credential, &connection.credential)
        }) {
            continue;
        }
        unique.push(connection);
    }
    unique
}

fn same_connection_identity(left: &JumpServerCredential, right: &JumpServerCredential) -> bool {
    normalize_endpoint(&left.base_url) == normalize_endpoint(&right.base_url)
        && left.ssh_port == right.ssh_port
        && left.username == right.username
}

fn normalize_endpoint(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}

pub(super) fn build_jumpserver_profile(
    session: &JumpServerSession,
    detail: &JumpServerAssetDetail,
    account_id: &str,
) -> Result<SshProfile> {
    detail
        .asset
        .validate_id()
        .map_err(DomainError::InvalidConfig)?;
    if !detail.asset.active {
        return Err(DomainError::InvalidConfig("该资产已停用".into()));
    }
    if !detail.ssh_enabled {
        return Err(DomainError::InvalidConfig("该资产未开放 SSH 协议".into()));
    }
    let account = detail
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| DomainError::NotFound("选中的授权账号已失效，请重新选择".into()))?;
    account
        .validate_for_direct_login()
        .map_err(DomainError::InvalidConfig)?;

    let mut profile = SshProfile::new(detail.asset.name.clone(), session.ssh_host.clone());
    profile.origin = SshProfileOrigin::JumpServer;
    profile.port = Some(session.ssh_port);
    profile.username = format!("{}#{}#{}", session.username, account.name, detail.asset.id);
    profile.auth_mode = SshAuthMode::Password;
    profile.password = session.password.clone();
    profile.validate().map_err(DomainError::InvalidConfig)?;
    Ok(profile)
}
