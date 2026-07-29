//! SSH 工具 headless 渲染回归测试。
#![allow(clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use gpui::{
    AppContext as _, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px,
    size,
};
use ramag_app::SshService;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, QueryRecord, QueryRecordId, RemoteDirectory, RemoteEntry,
    RemoteEntryKind, SshAuthMode, SshCapability, SshLaunchCommand, SshProfile, SshProfileId,
    SshProgressFn, SshWorkspacePreference, SshWorkspaceState, TransferCancellation,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{SshDriver, Storage};

use super::SshView;
use super::model::{Notice, ViewMode};
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
            env: Default::default(),
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

    async fn read_file_preview(
        &self,
        _profile: &SshProfile,
        _path: &str,
    ) -> Result<ramag_domain::entities::RemoteFilePreview> {
        Ok(ramag_domain::entities::RemoteFilePreview {
            bytes: b"preview".to_vec(),
            total_bytes: 7,
            truncated: false,
        })
    }

    async fn read_file_chunk(
        &self,
        _profile: &SshProfile,
        _path: &str,
        _position: ramag_domain::entities::RemoteFileChunkPosition,
    ) -> Result<ramag_domain::entities::RemoteFileChunk> {
        Ok(ramag_domain::entities::RemoteFileChunk {
            bytes: b"readme".to_vec(),
            offset: 0,
            total_bytes: 6,
        })
    }

    async fn save_file(
        &self,
        _profile: &SshProfile,
        _path: &str,
        _expected: &[u8],
        _contents: &[u8],
    ) -> Result<()> {
        Ok(())
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

    async fn download_directory(
        &self,
        _profile: &SshProfile,
        _remote_path: &str,
        _local_path: &Path,
        _overwrite: ramag_domain::entities::OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
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
    assert!(
        cx.debug_bounds("ssh-profile-executable-field-input")
            .is_some(),
        "高级选项应默认展开"
    );
    let openssh_status = cx
        .debug_bounds("ssh-openssh-status")
        .expect("OpenSSH 状态应显示在路径字段标题行");
    assert!(
        cx.debug_bounds("ssh-openssh-label").is_some(),
        "OpenSSH 状态前应显示本机 SSH 标题"
    );
    assert!(
        cx.debug_bounds("ssh-production-label").is_some(),
        "生产模式标题应参与布局"
    );
    assert!(name.origin.y < host.origin.y, "名称应显示在 Host 上方");
    let executable = cx
        .debug_bounds("ssh-profile-executable-field")
        .expect("OpenSSH 路径字段应参与布局");
    assert!(
        openssh_status.origin.y >= executable.origin.y,
        "OpenSSH 状态应移入路径字段"
    );

    let password_auth = cx
        .debug_bounds("ssh-auth-password")
        .expect("password auth button should be rendered");
    let system_auth = cx
        .debug_bounds("ssh-auth-system")
        .expect("system auth button should be rendered");
    assert!(
        password_auth.origin.x < system_auth.origin.x,
        "密码认证应显示在系统认证前"
    );

    form.read_with(cx, |form, cx| {
        assert_eq!(form.auth_mode, SshAuthMode::Password);
        assert!(!form.is_dirty(cx));
    });
    assert!(
        cx.debug_bounds("ssh-profile-password-field-input")
            .is_some(),
        "新建连接默认应显示密码输入框"
    );
    form.update(cx, |form, cx| form.set_auth_mode(SshAuthMode::System, cx));
    cx.run_until_parked();
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
        workspace.entries = Arc::new(vec![
            RemoteEntry {
                name: "readme.txt".into(),
                path: "/home/alice/readme.txt".into(),
                kind: RemoteEntryKind::File,
                size: 1536,
                permissions: Some(0o100644),
                modified_at: None,
            },
            RemoteEntry {
                name: "logs".into(),
                path: "/home/alice/logs".into(),
                kind: RemoteEntryKind::Directory,
                size: 0,
                permissions: Some(0o40755),
                modified_at: None,
            },
        ]);
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert_eq!(view.view_mode, ViewMode::Workspace);
        assert_eq!(view.workspaces.len(), 1);
        assert!(
            view.workspaces[0].connection_started,
            "恢复的活动工作区应自动重连"
        );
        assert!(view.workspaces[0].terminals.is_empty());
        assert_eq!(view.workspaces[0].entries.len(), 2);
    });
    let file_browser = cx
        .debug_bounds("ssh-file-browser")
        .expect("file browser should be rendered");
    assert_eq!(
        file_browser.size.width,
        px(280.0),
        "目录栏默认宽度应与数据库侧栏一致"
    );
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.workspace_resize.update(cx, |state, cx| {
                state.resize_panel(0, px(360.0), window, cx);
            });
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("ssh-file-browser")
            .expect("拖动后文件树应参与布局")
            .size
            .width,
        px(360.0),
        "文件树宽度应受分隔条状态控制"
    );
    assert!(
        cx.debug_bounds("ssh-directory-summary").is_some(),
        "目录底部应显示文件与目录数量"
    );
    assert!(
        cx.debug_bounds("ssh-directory-breadcrumb").is_some(),
        "目录顶部应显示可滚动路径"
    );
    assert!(
        cx.debug_bounds("ssh-directory-path-label").is_some(),
        "路径面包屑前应显示路径标签"
    );
    assert!(
        cx.debug_bounds("ssh-directory-search").is_some(),
        "目录操作栏应以搜索框开头"
    );
    let entry = cx
        .debug_bounds("sftp-entry-0")
        .expect("remote entry should be rendered");
    assert_eq!(
        entry.size.height,
        px(28.0),
        "目录项高度应与数据库树保持一致"
    );
    view.update(cx, |view, cx| {
        let workspace = view
            .workspace_mut(&profile.id)
            .expect("workspace should remain available");
        workspace.sftp_loading = true;
        workspace.directory_loading_path = Some("/home/alice/logs".into());
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("sftp-entry-loading-1").is_some(),
        "正在打开的目录行应显示加载图标"
    );
    view.update(cx, |view, cx| {
        let workspace = view
            .workspace_mut(&profile.id)
            .expect("workspace should remain available");
        workspace.sftp_loading = false;
        workspace.directory_loading_path = None;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("sftp-entry-loading-1").is_none(),
        "目录请求结束后应清除加载图标"
    );
    let entry_point = point(
        entry.origin.x + px(8.0),
        entry.origin.y + entry.size.height / 2.0,
    );
    cx.simulate_mouse_down(entry_point, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(entry_point, MouseButton::Right, Modifiers::default());
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.workspaces[0].selected_path.as_deref(),
            Some("/home/alice/readme.txt"),
            "右键文件时应同步选中对应条目"
        )
    });
    cx.simulate_keystrokes("escape");
    assert_eq!(service_for_assert.transfer_tasks().len(), 1);
    assert!(
        cx.debug_bounds("ssh-directory-transfers").is_some(),
        "目录操作应提供明确的传输入口"
    );
    let workspace_before_notice = cx
        .debug_bounds("ssh-workspace-main")
        .expect("workspace should be rendered");
    view.update(cx, |view, cx| {
        view.notice = Some(Notice::error("测试通知"));
        cx.notify();
    });
    cx.run_until_parked();
    let workspace_after_notice = cx
        .debug_bounds("ssh-workspace-main")
        .expect("workspace should remain rendered");
    assert_eq!(
        workspace_after_notice, workspace_before_notice,
        "通知应使用浮层，不应挤压工作区布局"
    );
    view.read_with(cx, |view, _| {
        assert!(view.notice.is_none(), "通知应移交给全局消息层")
    });
    assert!(
        cx.debug_bounds("ssh-transfer-panel").is_none(),
        "传输面板默认不应打开"
    );

    view.update(cx, |view, cx| view.toggle_transfer_panel(cx));
    cx.run_until_parked();
    let workspace = cx
        .debug_bounds("ssh-workspace-main")
        .expect("workspace should be rendered");
    let transfers = cx
        .debug_bounds("ssh-transfer-panel")
        .expect("触发传输入口后应显示面板");
    assert!(
        transfers.origin.x > workspace.origin.x + workspace.size.width / 2.0,
        "传输面板应悬浮在工作区右侧"
    );
    assert!(
        transfers.size.width <= px(520.0),
        "传输面板不应覆盖过多工作区：{:?}",
        transfers.size
    );

    view.update(cx, |view, cx| view.hide_transfer_panel(cx));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("ssh-transfer-panel").is_none(),
        "收起后不应继续占用工作区"
    );

    view.update(cx, |view, cx| {
        view.workspace_mut(&profile.id)
            .expect("workspace should remain available")
            .sftp_error = Some("打开远程目录：权限不足".into());
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("ssh-directory-direct").is_some(),
        "目录无权列出时应允许直达已知路径"
    );
}

#[gpui::test]
fn directory_search_state_is_isolated_by_workspace(cx: &mut TestAppContext) {
    let first = profile();
    let mut second = SshProfile::new("staging", "staging.example");
    second.initial_directory = Some("/srv/app".into());
    let preference = SshWorkspacePreference {
        workspaces: vec![
            SshWorkspaceState {
                profile_id: first.id.clone(),
                last_remote_path: "/home/alice".into(),
            },
            SshWorkspaceState {
                profile_id: second.id.clone(),
                last_remote_path: "/srv/app".into(),
            },
        ],
        active_profile_id: Some(first.id.clone()),
    };
    let (view, cx) = add_ssh_window(
        cx,
        service(vec![first.clone(), second.clone()], Some(preference)),
    );
    cx.run_until_parked();
    view.update(cx, |view, _| {
        view.workspace_mut(&first.id)
            .expect("首个工作区应恢复")
            .directory_query = "logs".into();
    });

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.select_workspace(second.id.clone(), window, cx);
        });
    });
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
        assert_eq!(view.directory_search.read(cx).value(), "");
        assert_eq!(
            view.workspaces
                .iter()
                .find(|workspace| workspace.profile_id() == &first.id)
                .expect("首个工作区应保留")
                .directory_query,
            "logs"
        );
    });

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.select_workspace(first.id.clone(), window, cx);
        });
    });
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
        assert_eq!(view.directory_search.read(cx).value(), "logs");
    });
}
