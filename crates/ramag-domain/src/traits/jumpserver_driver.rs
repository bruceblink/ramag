//! JumpServer HTTP API 基础设施抽象。

use async_trait::async_trait;

use crate::entities::{
    JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog, JumpServerCredential,
    JumpServerSession,
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
}
