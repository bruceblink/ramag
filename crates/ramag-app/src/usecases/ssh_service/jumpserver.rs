//! JumpServer 资源到本地 SSH 配置的用例编排。

use ramag_domain::entities::{
    JumpServerAsset, JumpServerAssetDetail, JumpServerCredential, JumpServerSession, SshAuthMode,
    SshProfile,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::JumpServerDriver;

use super::SshService;

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

    pub async fn list_jumpserver_assets(
        &self,
        session: &JumpServerSession,
    ) -> Result<Vec<JumpServerAsset>> {
        self.jumpserver_driver()?.list_assets(session).await
    }

    pub async fn jumpserver_asset_detail(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail> {
        self.jumpserver_driver()?.asset_detail(session, asset).await
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
    profile.port = Some(session.ssh_port);
    profile.username = format!("{}#{}#{}", session.username, account.name, detail.asset.id);
    profile.auth_mode = SshAuthMode::Password;
    profile.password = session.password.clone();
    profile.validate().map_err(DomainError::InvalidConfig)?;
    Ok(profile)
}
