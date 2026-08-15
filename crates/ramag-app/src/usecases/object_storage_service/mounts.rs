//! 用户配置的 Bucket 挂载转换。

use ramag_domain::entities::{
    CloudProvider, HttpsEndpoint, ObjectStorageAccount, ObjectStorageMount,
};
use ramag_domain::error::Result;

use super::{ObjectStorageMountResult, ObjectStorageService};

impl ObjectStorageService {
    pub async fn list_mounts(
        &self,
        account_id: &ramag_domain::entities::ObjectStorageAccountId,
    ) -> Result<ObjectStorageMountResult> {
        let gate = self.account_gate(account_id);
        let _account_guard = gate.read_owned().await;
        let account = self.get_account(account_id).await?;
        let mounts = configured_mounts(&account)?;
        if mounts.is_empty() {
            return Err(ramag_domain::error::DomainError::InvalidConfig(
                "此账号未配置 Bucket，请编辑账号后至少添加一个挂载".into(),
            ));
        }
        Ok(ObjectStorageMountResult { mounts })
    }
}

pub fn configured_mounts(account: &ObjectStorageAccount) -> Result<Vec<ObjectStorageMount>> {
    let mut mounts = Vec::with_capacity(account.manual_buckets.len());
    for bucket in &account.manual_buckets {
        mounts.push(ObjectStorageMount {
            id: bucket.id.clone(),
            account_id: account.id.clone(),
            bucket: bucket.name.clone(),
            region: bucket.region.clone(),
            endpoint: official_endpoint(account.provider, &bucket.region)?,
            root_prefix: bucket.root_prefix.clone(),
            created_at: None,
            storage_class: None,
        });
    }
    mounts.sort_by(|left, right| {
        (&left.bucket, &left.region, &left.root_prefix).cmp(&(
            &right.bucket,
            &right.region,
            &right.root_prefix,
        ))
    });
    Ok(mounts)
}

fn official_endpoint(provider: CloudProvider, region: &str) -> Result<HttpsEndpoint> {
    let value = match provider {
        CloudProvider::TencentCos => format!("https://cos.{region}.myqcloud.com"),
        CloudProvider::AliyunOss => format!("https://oss-{region}.aliyuncs.com"),
    };
    HttpsEndpoint::parse_official(provider, &value)
        .map_err(ramag_domain::error::DomainError::InvalidConfig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{ManualBucket, SecretString};

    #[test]
    fn same_bucket_can_have_independent_root_mounts() {
        let mut account = ObjectStorageAccount::new("test", CloudProvider::AliyunOss);
        account.access_key_id = SecretString::new("id");
        account.access_key_secret = SecretString::new("secret");
        let mut first = ManualBucket::new("valid-bucket", "cn-hangzhou");
        first.root_prefix = Some("team-a/".into());
        let mut second = ManualBucket::new("valid-bucket", "cn-hangzhou");
        second.root_prefix = Some("team-b/".into());
        account.manual_buckets = vec![first, second];
        let mounts = configured_mounts(&account).unwrap();
        assert_eq!(mounts.len(), 2);
        assert_ne!(mounts[0].id, mounts[1].id);
    }
}
