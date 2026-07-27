//! SSH 工具 headless 渲染回归测试。
#![allow(clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};
use ramag_app::SshService;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, QueryRecord, QueryRecordId, RemoteDirectory, RemoteEntry,
    RemoteEntryKind, SshAuthMode, SshCapability, SshLaunchCommand, SshProfile, SshProfileId,
    SshProgressFn, SshWorkspacePreference, SshWorkspaceState, TransferCancellation,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{SshDriver, Storage};

use super::SshView;
use super::model::ViewMode;
use super::profile_dialog::SshProfileFormPanel;

struct MockStorage {
    profiles: Vec<SshProfile>,
    workspace_preference: Option<String>,
}

#[async_trait]
impl Storage for MockStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(Vec::new())
    }

    async fn get_connection(&self, _id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }

    async fn save_connection(&self, _config: &ConnectionConfig) -> Result<()> {
        Ok(())
    }

    async fn delete_connection(&self, _id: &ConnectionId) -> Result<()> {
        Ok(())
    }

    async fn list_ssh_profiles(&self) -> Result<Vec<SshProfile>> {
        Ok(self.profiles.clone())
    }

    async fn append_history(&self, _record: &QueryRecord) -> Result<()> {
        Ok(())
    }

    async fn list_history(
        &self,
        _connection_id: Option<&ConnectionId>,
        _limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        Ok(Vec::new())
    }

    async fn delete_history(&self, _id: &QueryRecordId) -> Result<()> {
        Ok(())
    }

    async fn clear_history(&self, _connection_id: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }

    async fn get_preference(&self, _key: &str) -> Result<Option<String>> {
        Ok(self.workspace_preference.clone())
    }

    async fn set_preference(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
}

struct MockSshDriver;

#[async_trait]
impl SshDriver for MockSshDriver {
    async fn probe(&self, _custom_path: Option<&str>) -> Result<SshCapability> {
        Ok(SshCapability {
            executable: "/mock/ssh".into(),
            version: "OpenSSH_mock".into(),
        })
    }

    async fn terminal_command(&self, profile: &SshProfile) -> Result<SshLaunchCommand> {
        Ok(SshLaunchCommand {
            profile_id: profile.id.clone(),
            program: "/mock/ssh".into(),
            args: vec!["--".into(), profile.host.clone()],
        })
    }

    async fn report_terminal_launch_failure(&self, _executable: &str) {}

    async fn test_connection(&self, _profile: &SshProfile) -> Result<()> {
        Ok(())
    }

    async fn list_directory(&self, _profile: &SshProfile, path: &str) -> Result<RemoteDirectory> {
        Ok(RemoteDirectory {
            path: path.into(),
            entries: Vec::new(),
        })
    }

    async fn create_directory(&self, _profile: &SshProfile, _path: &str) -> Result<()> {
        Ok(())
    }

    async fn rename(&self, _profile: &SshProfile, _old_path: &str, _new_path: &str) -> Result<()> {
        Ok(())
    }

    async fn remove(
        &self,
        _profile: &SshProfile,
        _path: &str,
        _kind: RemoteEntryKind,
    ) -> Result<()> {
        Ok(())
    }

    async fn upload(
        &self,
        _profile: &SshProfile,
        _local_path: &Path,
        _remote_path: &str,
        _overwrite: ramag_domain::entities::OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Err(DomainError::NotImplemented("mock upload".into()))
    }

    async fn download(
        &self,
        _profile: &SshProfile,
        _remote_path: &str,
        _local_path: &Path,
        _overwrite: ramag_domain::entities::OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Err(DomainError::NotImplemented("mock download".into()))
    }

    async fn disconnect(&self, _profile_id: &SshProfileId) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

fn profile() -> SshProfile {
    let mut profile = SshProfile::new("production", "server.example");
    profile.username = "alice".into();
    profile.initial_directory = Some("/home/alice".into());
    profile
}

fn service(
    profiles: Vec<SshProfile>,
    preference: Option<SshWorkspacePreference>,
) -> Arc<SshService> {
    let workspace_preference = preference
        .map(|value| serde_json::to_string(&value).expect("workspace preference should serialize"));
    Arc::new(SshService::new(
        Arc::new(MockSshDriver),
        Arc::new(MockStorage {
            profiles,
            workspace_preference,
        }),
    ))
}

fn add_ssh_window(
    cx: &mut TestAppContext,
    service: Arc<SshService>,
) -> (Entity<SshView>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut view = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let ssh_view = cx.new(|cx| SshView::new(service, window, cx));
        view = Some(ssh_view.clone());
        gpui_component::Root::new(ssh_view, window, cx)
    });
    (view.expect("SshView should be initialized"), visual_cx)
}

fn add_ssh_form_window(
    cx: &mut TestAppContext,
    service: Arc<SshService>,
) -> (Entity<SshProfileFormPanel>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut form = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let capability = Some(Ok(SshCapability {
            executable: "/mock/ssh".into(),
            version: "OpenSSH_mock".into(),
        }));
        let entity = cx.new(|cx| SshProfileFormPanel::new(service, None, capability, window, cx));
        form = Some(entity.clone());
        gpui_component::Root::new(entity, window, cx)
    });
    (
        form.expect("SshProfileFormPanel should be initialized"),
        visual_cx,
    )
}

#[gpui::test]
fn connection_manager_renders_without_openssh_side_effects(cx: &mut TestAppContext) {
    let (view, cx) = add_ssh_window(cx, service(vec![profile()], None));
    cx.run_until_parked();
    view.update(cx, |_, cx| cx.notify());
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert_eq!(view.view_mode, ViewMode::Manager);
        assert!(!view.loading_profiles);
        assert!(matches!(view.default_capability, Some(Ok(_))));
    });
    let search = cx
        .debug_bounds("ssh-profile-search")
        .expect("SSH 搜索区应参与布局");
    let row = cx
        .debug_bounds("ssh-profile-row-0")
        .expect("SSH 连接行应参与布局");
    assert!(
        search.size.width > px(300.0),
        "搜索区宽度异常：{:?}",
        search.size
    );
    assert!(
        row.size.width <= px(1080.0),
        "连接行应遵守限宽：{:?}",
        row.size
    );
    assert!(
        row.size.height <= px(48.0),
        "连接行不应变成卡片：{:?}",
        row.size
    );
}

#[gpui::test]
fn profile_form_inputs_keep_dialog_width_instead_of_collapsing(cx: &mut TestAppContext) {
    let (form, cx) = add_ssh_form_window(cx, service(Vec::new(), None));
    cx.simulate_resize(size(px(720.0), px(800.0)));
    cx.run_until_parked();

    let name = cx
        .debug_bounds("ssh-profile-name-field-input")
        .expect("name input container should be rendered");
    let host = cx
        .debug_bounds("ssh-profile-host-field-input")
        .expect("host input container should be rendered");
    let port = cx
        .debug_bounds("ssh-profile-port-field-input")
        .expect("port input container should be rendered");
    assert!(
        name.size.width > px(300.0),
        "名称输入框宽度异常：{:?}",
        name.size
    );
    assert!(
        host.size.width > px(300.0),
        "主机输入框宽度异常：{:?}",
        host.size
    );
    assert!(
        port.size.width >= px(100.0),
        "端口输入框宽度异常：{:?}",
        port.size
    );

    form.read_with(cx, |form, cx| assert!(!form.is_dirty(cx)));
    form.update(cx, |form, cx| form.set_auth_mode(SshAuthMode::KeyFile, cx));
    form.read_with(cx, |form, cx| assert!(form.is_dirty(cx)));
}

#[gpui::test]
fn restored_workspace_renders_files_terminal_placeholder_and_transfer(cx: &mut TestAppContext) {
    let profile = profile();
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile.id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile.id.clone()),
    };
    let service = service(vec![profile.clone()], Some(preference));
    let local_path = std::env::temp_dir().join("ramag-render-download.txt");
    service
        .enqueue_download(&profile, "/home/alice/readme.txt", &local_path)
        .expect("waiting transfer should enqueue");

    let service_for_assert = service.clone();
    let (view, cx) = add_ssh_window(cx, service);
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        let workspace = view
            .workspace_mut(&profile.id)
            .expect("workspace should be restored");
        workspace.entries = Arc::new(vec![RemoteEntry {
            name: "readme.txt".into(),
            path: "/home/alice/readme.txt".into(),
            kind: RemoteEntryKind::File,
            size: 1536,
            permissions: Some(0o100644),
            modified_at: None,
        }]);
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert_eq!(view.view_mode, ViewMode::Workspace);
        assert_eq!(view.workspaces.len(), 1);
        assert!(view.workspaces[0].terminals.is_empty());
        assert_eq!(view.workspaces[0].entries.len(), 1);
    });
    assert_eq!(service_for_assert.transfer_tasks().len(), 1);
}
