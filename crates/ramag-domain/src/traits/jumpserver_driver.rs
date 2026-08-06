//! JumpServer HTTP API 基础设施抽象。

use async_trait::async_trait;

use crate::entities::{
    JumpServerAccount, JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog,
    JumpServerCredential, JumpServerSession,
};
use crate::error::Result;

#[async_trait]
pub trait JumpServerDriver: Send + Sync {
    async fn authenticate(&self, credential: &JumpServerCredential) -> Result<JumpServerSession>;

    async fn load_catalog(&self, session: &JumpServerSession) -> Result<JumpServerCatalog>;

    async fn asset_detail(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail>;

    /// 创建一次性 RDP Web 连接令牌，并返回可交给系统浏览器的会话地址。
    async fn create_rdp_web_session(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
        account: &JumpServerAccount,
    ) -> Result<String>;
}
