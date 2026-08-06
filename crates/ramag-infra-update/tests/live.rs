//! GitHub 真实 Release 检查；默认跳过，避免普通测试依赖网络。
#![allow(clippy::expect_used, clippy::panic)]

use ramag_domain::traits::UpdateDriver as _;
use ramag_infra_update::GitHubUpdateDriver;

#[test]
fn latest_release_check_works_without_rest_api() {
    if std::env::var_os("RAMAG_TEST_UPDATE_LIVE").is_none() {
        return;
    }
    let driver = GitHubUpdateDriver::new(env!("CARGO_PKG_VERSION"))
        .expect("update driver should initialize");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should initialize");
    let release = runtime
        .block_on(driver.latest_stable_release())
        .expect("latest release should be reachable");

    assert!(!release.version.is_empty());
    assert_eq!(release.tag_name, format!("v{}", release.version));
    assert!(
        release
            .release_url
            .starts_with("https://github.com/tools-rs/ramag/releases/tag/")
    );
}
