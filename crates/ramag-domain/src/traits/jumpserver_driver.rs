//! JumpServer HTTP API 基础设施抽象。

use async_trait::async_trait;

use crate::entities::{
    JumpServerAsset, JumpServerAssetDetail, JumpServerCredential, JumpServerSession,
};
use crate::error::Result;

#[async_trait]
pub trait JumpServerDriver: Send + Sync {
    async fn authenticate(&self, credential: &JumpServerCredential) -> Result<JumpServerSession>;

    async fn list_assets(&self, session: &JumpServerSession) -> Result<Vec<JumpServerAsset>>;

    async fn asset_detail(
        &self,
        session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail>;
}
