use super::*;

fn valid_account() -> ObjectStorageAccount {
    let mut account = ObjectStorageAccount::new("production", CloudProvider::TencentCos);
    account.access_key_id = SecretString::new("AKIDEXAMPLE");
    account.access_key_secret = SecretString::new("secret-value");
    account.manual_buckets = vec![ManualBucket::new("logs-1250000000", "ap-guangzhou")];
    account
}

#[test]
fn account_defaults_to_writable_and_redacts_debug() {
    let account = valid_account();
    assert!(!account.read_only);
    let debug = format!("{account:?}");
    assert!(!debug.contains("AKIDEXAMPLE"));
    assert!(!debug.contains("secret-value"));
    assert!(account.validate().is_ok());
}

#[test]
fn rejects_incomplete_credentials() {
    let mut account = valid_account();
    account.access_key_secret = SecretString::new("");
    assert!(matches!(account.validate(), Err(error) if error.contains("不能为空")));
}

#[test]
fn rejects_account_without_configured_bucket() {
    let mut account = valid_account();
    account.manual_buckets.clear();
    assert!(matches!(account.validate(), Err(error) if error.contains("至少添加一个 Bucket")));
}

#[test]
fn official_endpoint_rejects_custom_hosts_and_redirect_shapes() {
    assert!(
        HttpsEndpoint::parse_official(
            CloudProvider::TencentCos,
            "https://cos.ap-guangzhou.myqcloud.com"
        )
        .is_ok()
    );
    assert!(
        HttpsEndpoint::parse_official(
            CloudProvider::AliyunOss,
            "https://oss-cn-hangzhou.aliyuncs.com"
        )
        .is_ok()
    );
    for endpoint in [
        "http://cos.ap-guangzhou.myqcloud.com",
        "https://evil.example.com",
        "https://oss-cn-hangzhou.aliyuncs.com@evil.example",
        "https://oss-cn-hangzhou.aliyuncs.com/path",
        "https://bucket.oss-cn-hangzhou.aliyuncs.com",
    ] {
        assert!(HttpsEndpoint::parse_official(CloudProvider::AliyunOss, endpoint).is_err());
    }
    assert!(
        HttpsEndpoint::parse_official(
            CloudProvider::TencentCos,
            "https://cos.ap-guangzhou.extra.myqcloud.com"
        )
        .is_err()
    );
}

#[test]
fn unsafe_keys_are_never_marked_operable() {
    for key in [
        "/leading",
        "trailing ",
        "a//b",
        "a/../b",
        "a/./b",
        "folder/",
        "line\nbreak",
        "rtl\u{202e}name",
    ] {
        assert!(!is_opendal_safe_key(key), "{key:?}");
    }
    for key in ["plain.txt", "目录/文件.txt", "a..b", "percent%2Fname"] {
        assert!(is_opendal_safe_key(key), "{key:?}");
    }
}

#[test]
fn root_prefix_requires_safe_relative_directory_shape() {
    assert!(validate_root_prefix("team/reports/").is_ok());
    assert!(validate_root_prefix("team/reports").is_err());
    assert!(validate_root_prefix("/team/reports/").is_err());
    assert!(validate_root_prefix("team//reports/").is_err());
}

#[test]
fn list_query_keeps_directory_and_name_prefix_separate() -> Result<(), String> {
    let query = ObjectListQuery::new("reports/2026/", "aug")?;
    assert_eq!(query.directory_prefix(), "reports/2026/");
    assert_eq!(query.name_prefix(), "aug");
    assert_eq!(query.list_prefix(), "reports/2026/aug");
    assert!(ObjectListQuery::new("reports/", "../secret").is_err());
    assert!(ObjectListQuery::new("reports/", "bad/name").is_err());
    Ok(())
}

#[test]
fn bucket_and_region_use_shared_official_character_subset() {
    assert!(validate_bucket_name("logs-1234567890").is_ok());
    assert!(validate_bucket_name("UPPER").is_err());
    assert!(validate_bucket_name("-leading").is_err());
    assert!(validate_region("ap-guangzhou").is_ok());
    assert!(validate_region("ap_guangzhou").is_err());
    assert!(validate_bucket_name_for_provider(CloudProvider::AliyunOss, &"a".repeat(64)).is_err());
}

#[test]
fn account_revision_saturates_instead_of_wrapping() {
    let mut account = valid_account();
    account.revision = u64::MAX;
    assert_eq!(account.next_revision(), u64::MAX);
}

#[test]
fn object_storage_user_message_includes_safe_provider_diagnostics() {
    let error = crate::error::ObjectStorageError::new(
        crate::error::ObjectStorageErrorCategory::Provider,
        "list",
        "云服务失败",
    )
    .with_provider_details(Some("ServiceBusy".into()), Some("request-123".into()));
    let message = error.user_message();
    assert!(message.contains("ServiceBusy"));
    assert!(message.contains("request-123"));
}
