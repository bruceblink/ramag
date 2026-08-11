//! OpenDAL Operator 的有界缓存与账号 revision 隔离。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use opendal::{Operator, services};
use parking_lot::Mutex;
use ramag_domain::entities::{
    CloudProvider, ObjectCapabilities, ObjectStorageAccountId, ObjectStorageAccountSnapshot,
    ObjectStorageMount,
};
use ramag_domain::error::ObjectStorageResult;

use crate::errors::{invalid, map_opendal};
use crate::transport::apply_transport;

const MAX_CACHED_OPERATORS: usize = 32;

#[derive(Default)]
struct CacheState {
    operators: HashMap<String, Arc<Operator>>,
    lru: VecDeque<String>,
    minimum_revisions: HashMap<ObjectStorageAccountId, u64>,
}

#[derive(Default)]
pub struct OperatorCache {
    state: Mutex<CacheState>,
}

impl OperatorCache {
    pub fn get(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
    ) -> ObjectStorageResult<Arc<Operator>> {
        if account.id != mount.account_id {
            return Err(invalid("operator", "挂载点不属于当前账号"));
        }
        mount
            .validate_for_provider(account.provider)
            .map_err(|error| invalid("operator", error))?;
        let identity = mount.operator_identity(account.revision);
        let mut state = self.state.lock();
        let minimum = state
            .minimum_revisions
            .get(&account.id)
            .copied()
            .unwrap_or(0);
        if account.revision < minimum {
            return Err(invalid("operator", "账号配置已更新，请刷新后重试"));
        }
        if let Some(operator) = state.operators.get(&identity).cloned() {
            touch(&mut state.lru, &identity);
            return Ok(operator);
        }

        let operator = Arc::new(build_operator(account, mount)?);
        state.operators.insert(identity.clone(), operator.clone());
        touch(&mut state.lru, &identity);
        while state.operators.len() > MAX_CACHED_OPERATORS {
            if let Some(oldest) = state.lru.pop_front() {
                state.operators.remove(&oldest);
            }
        }
        Ok(operator)
    }

    pub fn capabilities(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
    ) -> ObjectStorageResult<ObjectCapabilities> {
        let operator = self.get(account, mount)?;
        let capability = operator.info().capability();
        Ok(ObjectCapabilities {
            stat: capability.stat,
            read: capability.read,
            write: capability.write && !account.read_only,
            delete: capability.delete && !account.read_only,
            list: capability.list,
            atomic_create: capability.write_with_if_not_exists,
        })
    }

    pub fn invalidate(&self, account_id: &ObjectStorageAccountId, minimum_revision: u64) {
        let mut state = self.state.lock();
        state
            .minimum_revisions
            .entry(account_id.clone())
            .and_modify(|current| *current = (*current).max(minimum_revision))
            .or_insert(minimum_revision);
        let prefix = format!("{account_id}:");
        state.operators.retain(|key, _| !key.starts_with(&prefix));
        state.lru.retain(|key| !key.starts_with(&prefix));
    }

    pub fn clear(&self) {
        let mut state = self.state.lock();
        state.operators.clear();
        state.lru.clear();
    }
}

fn touch(lru: &mut VecDeque<String>, identity: &str) {
    lru.retain(|current| current != identity);
    lru.push_back(identity.to_string());
}

fn build_operator(
    account: &ObjectStorageAccountSnapshot,
    mount: &ObjectStorageMount,
) -> ObjectStorageResult<Operator> {
    let root = mount.root_prefix.as_deref().unwrap_or("");
    match account.provider {
        CloudProvider::TencentCos => {
            let builder = services::Cos::default()
                .bucket(&mount.bucket)
                .endpoint(mount.endpoint.as_str())
                .secret_id(account.access_key_id.expose_secret())
                .secret_key(account.access_key_secret.expose_secret())
                .root(root)
                .disable_config_load();
            let operator =
                Operator::new(builder).map_err(|error| map_opendal("operator", error))?;
            apply_transport(operator, account, mount)
        }
        CloudProvider::AliyunOss => {
            let builder = services::Oss::default()
                .bucket(&mount.bucket)
                .endpoint(mount.endpoint.as_str())
                .access_key_id(account.access_key_id.expose_secret())
                .access_key_secret(account.access_key_secret.expose_secret())
                .root(root);
            let operator =
                Operator::new(builder).map_err(|error| map_opendal("operator", error))?;
            apply_transport(operator, account, mount)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{
        HttpsEndpoint, ObjectStorageAccount, ObjectStorageMountId, SecretString,
    };

    #[test]
    fn invalidation_rejects_stale_snapshot() {
        let mut account = ObjectStorageAccount::new("test", CloudProvider::TencentCos);
        account.access_key_id = SecretString::new("id");
        account.access_key_secret = SecretString::new("secret");
        let mount = ObjectStorageMount {
            id: ObjectStorageMountId::new(),
            account_id: account.id.clone(),
            bucket: "bucket-1234567890".into(),
            region: "ap-guangzhou".into(),
            endpoint: HttpsEndpoint::parse_official(
                CloudProvider::TencentCos,
                "https://cos.ap-guangzhou.myqcloud.com",
            )
            .unwrap(),
            root_prefix: None,
            created_at: None,
            storage_class: None,
        };
        let cache = OperatorCache::default();
        cache.invalidate(&account.id, 2);
        assert!(cache.get(&account.snapshot(), &mount).is_err());
    }
}
