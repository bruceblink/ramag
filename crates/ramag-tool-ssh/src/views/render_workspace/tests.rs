//! 拆分后的测试模块。

use super::*;
use ramag_domain::entities::{SshProfile, SshRemoteCapabilities};

#[test]
fn empty_jumpserver_windows_root_explains_that_no_drive_was_returned() {
    let mut profile = SshProfile::new("windows", "jump.example.com");
    profile.origin = SshProfileOrigin::JumpServer;
    let mut workspace = SshWorkspace::placeholder(profile, "/".into());
    workspace.directory_loaded = true;
    workspace.capabilities = Some(SshRemoteCapabilities {
        operating_system: RemoteOperatingSystem::Windows,
        ..SshRemoteCapabilities::default()
    });

    assert_eq!(empty_directory_message(&workspace), "未返回可访问盘符");
}
