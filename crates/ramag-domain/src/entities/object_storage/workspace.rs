//! 对象存储会话、导航状态与收藏夹偏好。

use serde::{Deserialize, Serialize};

use super::{ObjectStorageAccountId, ObjectStorageMountId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObjectStorageSessionPreference {
    pub open_account_ids: Vec<ObjectStorageAccountId>,
    pub active_account_id: Option<ObjectStorageAccountId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObjectStorageWorkspacePreference {
    pub active_account_id: Option<ObjectStorageAccountId>,
    pub workspaces: Vec<ObjectStorageWorkspaceState>,
    pub favorites: Vec<ObjectStorageFavorite>,
    #[serde(default)]
    pub show_mounts: bool,
    #[serde(default)]
    pub show_detail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectStorageWorkspaceState {
    pub account_id: ObjectStorageAccountId,
    pub mount_id: Option<ObjectStorageMountId>,
    pub prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectStorageFavorite {
    pub account_id: ObjectStorageAccountId,
    pub mount_id: ObjectStorageMountId,
    pub prefix: String,
}
