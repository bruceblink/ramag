//! SSH 工具 headless 渲染回归测试。
#![allow(clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::Duration;

use async_trait::async_trait;
use gpui::{
    AppContext as _, Entity, Focusable as _, Modifiers, MouseButton, TestAppContext,
    VisualTestContext, point, px, size,
};
use ramag_app::SshService;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DiagnosticCancellation, DiagnosticTermination,
    JumpServerAccount, JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog,
    JumpServerConnection, JumpServerCredential, JumpServerRdpSession, JumpServerRdpSessionHistory,
    JumpServerSession, QueryRecord, QueryRecordId, RemoteCapabilityState, RemoteDirectory,
    RemoteEntry, RemoteEntryKind, RemoteOperatingSystem, RemotePlatformPreference, RemoteShellKind,
    SftpNamespaceKind, SshAuthMode, SshCapability, SshDiagnosticOperation,
    SshDiagnosticProviderKind, SshDiagnosticResult, SshLaunchCommand, SshPathFavorites, SshProfile,
    SshProfileId, SshProfileOrigin, SshProgressFn, SshRemoteCapabilities, SshWorkspacePreference,
    SshWorkspaceState, TransferCancellation,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{JumpServerDriver, SshDriver, Storage};
#[cfg(unix)]
use ramag_terminal::{TerminalCommand, TerminalCore, TerminalView};

use super::SshView;
use super::jumpserver_dialog::JumpServerPanel;
use super::model::{Notice, TerminalTab, ViewMode};
use super::profile_dialog::SshProfileFormPanel;
use super::remote_session_dialog::RemoteSessionPanel;

struct MockStorage {
    profiles: Vec<SshProfile>,
    workspace_preference: Option<String>,
    preferences: Mutex<HashMap<String, String>>,
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

    async fn get_ssh_profile(&self, id: &SshProfileId) -> Result<Option<SshProfile>> {
        Ok(self
            .profiles
            .iter()
            .find(|profile| &profile.id == id)
            .cloned())
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

    async fn get_preference(&self, key: &str) -> Result<Option<String>> {
        if key == "ssh_workspaces_v1" {
            return Ok(self.workspace_preference.clone());
        }
        Ok(self
            .preferences
            .lock()
            .expect("mock preferences should not be poisoned")
            .get(key)
            .cloned())
    }

    async fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        self.preferences
            .lock()
            .expect("mock preferences should not be poisoned")
            .insert(key.into(), value.into());
        Ok(())
    }

    async fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        Ok(plain.to_vec())
    }

    async fn unseal(&self, cipher: &[u8]) -> Result<Vec<u8>> {
        Ok(cipher.to_vec())
    }
}

#[derive(Default)]
struct MockSshDriver {
    working_terminal: bool,
}

struct MockJumpServerDriver;

#[async_trait]
impl JumpServerDriver for MockJumpServerDriver {
    async fn authenticate(&self, credential: &JumpServerCredential) -> Result<JumpServerSession> {
        Ok(JumpServerSession {
            base_url: credential.base_url.clone(),
            ssh_host: "jump.example.com".into(),
            ssh_port: credential.ssh_port,
            username: credential.username.clone(),
            password: credential.password.clone(),
            token_keyword: "Bearer".into(),
            token: "token".into(),
            organizations: Vec::new(),
        })
    }

    async fn load_catalog(&self, _session: &JumpServerSession) -> Result<JumpServerCatalog> {
        Err(DomainError::NotImplemented("mock catalog".into()))
    }

    async fn asset_detail(
        &self,
        _session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail> {
        Ok(JumpServerAssetDetail {
            asset: asset.clone(),
            accounts: vec![JumpServerAccount {
                id: "account-1".into(),
                alias: "account-1".into(),
                name: "admin".into(),
                username: "Administrator".into(),
                has_secret: true,
                can_connect: true,
            }],
            ssh_enabled: false,
            rdp_web_enabled: true,
        })
    }

    async fn create_rdp_web_session(
        &self,
        _session: &JumpServerSession,
        _asset: &JumpServerAsset,
        _account: &JumpServerAccount,
    ) -> Result<String> {
        Ok(
            "https://jump.example.com/lion/connect?token=00000000-0000-0000-0000-000000000002"
                .into(),
        )
    }
}

#[async_trait]
impl SshDriver for MockSshDriver {
    async fn probe(&self, _custom_path: Option<&str>) -> Result<SshCapability> {
        Ok(SshCapability {
            executable: "/mock/ssh".into(),
            version: "OpenSSH_mock".into(),
        })
    }

    async fn terminal_command(
        &self,
        profile: &SshProfile,
        _initial_directory: Option<&str>,
    ) -> Result<SshLaunchCommand> {
        let (program, args) = if self.working_terminal {
            ("/bin/sh".into(), vec!["-c".into(), "exit 0".into()])
        } else {
            ("/mock/ssh".into(), vec!["--".into(), profile.host.clone()])
        };
        Ok(SshLaunchCommand {
            profile_id: profile.id.clone(),
            authorization_generation: 0,
            program,
            args,
            env: Default::default(),
        })
    }

    async fn report_terminal_launch_failure(&self, _executable: &str) {}

    async fn test_connection(&self, _profile: &SshProfile) -> Result<()> {
        Ok(())
    }

    async fn probe_remote_capabilities(
        &self,
        profile: &SshProfile,
    ) -> Result<SshRemoteCapabilities> {
        let windows = profile.remote_platform == RemotePlatformPreference::Windows;
        let operating_system = if windows {
            RemoteOperatingSystem::Windows
        } else {
            RemoteOperatingSystem::Linux
        };
        let namespace = if windows {
            SftpNamespaceKind::WindowsDrive
        } else {
            SftpNamespaceKind::Posix
        };
        let canonical_path = if windows {
            ramag_domain::entities::RemotePath::parse_server_canonical("C:/Users/Administrator")
                .unwrap()
        } else {
            ramag_domain::entities::RemotePath::parse_server_canonical("/").unwrap()
        };
        Ok(SshRemoteCapabilities {
            openssh_client: RemoteCapabilityState::Available,
            ssh_authentication: RemoteCapabilityState::Available,
            operating_system,
            shell: if windows {
                RemoteShellKind::Cmd
            } else {
                RemoteShellKind::Posix
            },
            ssh_execution: RemoteCapabilityState::Available,
            terminal: if profile.production {
                RemoteCapabilityState::BlockedByPolicy
            } else {
                RemoteCapabilityState::Available
            },
            sftp: RemoteCapabilityState::Available,
            sftp_namespace: namespace,
            sftp_canonical_path: Some(canonical_path),
            diagnostic: RemoteCapabilityState::Available,
            diagnostic_provider: Some(if windows {
                SshDiagnosticProviderKind::WindowsPowerShellV1
            } else {
                SshDiagnosticProviderKind::LinuxBuiltinV1
            }),
            ..SshRemoteCapabilities::default()
        })
    }

    async fn execute_diagnostic(
        &self,
        profile: &SshProfile,
        capabilities: &SshRemoteCapabilities,
        operation: &SshDiagnosticOperation,
        _cancellation: DiagnosticCancellation,
    ) -> Result<SshDiagnosticResult> {
        Ok(SshDiagnosticResult {
            profile_id: profile.id.clone(),
            operation: operation.kind().into(),
            operating_system: capabilities.operating_system,
            provider: capabilities
                .diagnostic_provider
                .unwrap_or(SshDiagnosticProviderKind::LinuxBuiltinV1),
            output: "ok".into(),
            exit_code: Some(0),
            termination: DiagnosticTermination::Completed,
            truncated: false,
            elapsed_millis: 1,
        })
    }

    async fn list_directory(&self, profile: &SshProfile, path: &str) -> Result<RemoteDirectory> {
        if profile.remote_platform == RemotePlatformPreference::Windows && matches!(path, "." | "/")
        {
            return Ok(RemoteDirectory {
                path: "/".into(),
                entries: ["C", "D"]
                    .into_iter()
                    .map(|drive| RemoteEntry {
                        name: format!("{drive}:"),
                        path: format!("/{drive}:/"),
                        kind: RemoteEntryKind::Directory,
                        size: 0,
                        permissions: None,
                        modified_at: None,
                    })
                    .collect(),
            });
        }
        let path = if path == "." { "/" } else { path };
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
        Arc::new(MockSshDriver::default()),
        Arc::new(MockStorage {
            profiles,
            workspace_preference,
            preferences: Mutex::new(HashMap::new()),
        }),
    ))
}

#[cfg(unix)]
fn service_with_working_terminal(
    profile: SshProfile,
    preference: SshWorkspacePreference,
) -> Arc<SshService> {
    Arc::new(SshService::new(
        Arc::new(MockSshDriver {
            working_terminal: true,
        }),
        Arc::new(MockStorage {
            profiles: vec![profile],
            workspace_preference: Some(
                serde_json::to_string(&preference).expect("workspace preference should serialize"),
            ),
            preferences: Mutex::new(HashMap::new()),
        }),
    ))
}

fn service_with_jumpserver() -> Arc<SshService> {
    Arc::new(
        SshService::new(
            Arc::new(MockSshDriver::default()),
            Arc::new(MockStorage {
                profiles: Vec::new(),
                workspace_preference: None,
                preferences: Mutex::new(HashMap::new()),
            }),
        )
        .with_jumpserver_driver(Arc::new(MockJumpServerDriver)),
    )
}

fn service_with_profile_rdp(profile: SshProfile) -> Arc<SshService> {
    let connection = JumpServerConnection {
        id: "00000000-0000-0000-0000-000000000001".into(),
        credential: JumpServerCredential {
            base_url: "https://jump.example.com/".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "password".into(),
        },
    };
    let json = serde_json::to_vec(&vec![connection]).expect("connection should serialize");
    let encoded = json
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let preferences = HashMap::from([(
        "ssh_jumpserver_connections_v2".into(),
        format!("enc-v1:{encoded}"),
    )]);
    Arc::new(
        SshService::new(
            Arc::new(MockSshDriver::default()),
            Arc::new(MockStorage {
                profiles: vec![profile],
                workspace_preference: None,
                preferences: Mutex::new(preferences),
            }),
        )
        .with_jumpserver_driver(Arc::new(MockJumpServerDriver)),
    )
}

fn service_with_rdp_history(history: &JumpServerRdpSessionHistory) -> Arc<SshService> {
    let json = serde_json::to_vec(history).expect("RDP history should serialize");
    let encoded = json
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let preferences = HashMap::from([(
        "ssh_jumpserver_rdp_sessions_v1".into(),
        format!("enc-v1:{encoded}"),
    )]);
    Arc::new(SshService::new(
        Arc::new(MockSshDriver::default()),
        Arc::new(MockStorage {
            profiles: Vec::new(),
            workspace_preference: None,
            preferences: Mutex::new(preferences),
        }),
    ))
}

fn rdp_session(index: u32, asset_name: &str) -> JumpServerRdpSession {
    JumpServerRdpSession {
        connection_id: "00000000-0000-0000-0000-000000000001".into(),
        jumpserver_url: "https://jump.example.com/".into(),
        asset_id: format!("00000000-0000-0000-0000-{index:012}"),
        org_id: "org-1".into(),
        asset_name: asset_name.into(),
        asset_address: format!("10.0.0.{index}"),
        asset_platform: "Windows".into(),
        account_id: format!("account-{index}"),
        account_name: "admin".into(),
        account_username: "Administrator".into(),
    }
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
    add_ssh_form_window_with_profile(cx, service, None)
}

fn add_ssh_form_window_with_profile(
    cx: &mut TestAppContext,
    service: Arc<SshService>,
    profile: Option<SshProfile>,
) -> (Entity<SshProfileFormPanel>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut form = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let capability = Some(Ok(SshCapability {
            executable: "/mock/ssh".into(),
            version: "OpenSSH_mock".into(),
        }));
        let entity =
            cx.new(|cx| SshProfileFormPanel::new(service, profile, capability, window, cx));
        form = Some(entity.clone());
        gpui_component::Root::new(entity, window, cx)
    });
    (
        form.expect("SshProfileFormPanel should be initialized"),
        visual_cx,
    )
}

fn add_jumpserver_panel_window(
    cx: &mut TestAppContext,
    service: Arc<SshService>,
) -> (Entity<JumpServerPanel>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut panel = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| JumpServerPanel::new(service, window, cx));
        panel = Some(entity.clone());
        gpui_component::Root::new(entity, window, cx)
    });
    (
        panel.expect("JumpServer panel should be initialized"),
        visual_cx,
    )
}

fn add_remote_session_panel_window(
    cx: &mut TestAppContext,
    service: Arc<SshService>,
) -> (Entity<RemoteSessionPanel>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut panel = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| RemoteSessionPanel::new(service, cx));
        panel = Some(entity.clone());
        gpui_component::Root::new(entity, window, cx)
    });
    (
        panel.expect("Remote session panel should be initialized"),
        visual_cx,
    )
}

#[gpui::test]
fn connection_manager_renders_without_openssh_side_effects(cx: &mut TestAppContext) {
    let mut imported = profile();
    imported.origin = SshProfileOrigin::JumpServer;
    imported.remote_platform = RemotePlatformPreference::Windows;
    imported.rdp_web_enabled = Some(true);
    imported.jumpserver_rdp_session = Some(rdp_session(1, "production"));
    let mut legacy = profile();
    legacy.name = "legacy".into();
    let (view, cx) = add_ssh_window(cx, service(vec![imported, legacy], None));
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
        cx.debug_bounds("open-remote-sessions").is_some(),
        "SSH 管理页应提供远程会话入口"
    );
    assert!(
        cx.debug_bounds("import-jumpserver-profile").is_some(),
        "SSH 管理页应提供 JumpServer 导入入口"
    );
    assert!(
        cx.debug_bounds("ssh-profile-jumpserver-icon-0").is_some(),
        "JumpServer 导入连接应显示官方图标"
    );
    assert!(
        cx.debug_bounds("ssh-profile-platform-0").is_some(),
        "SSH 连接行应显示系统类型"
    );
    assert!(
        cx.debug_bounds("ssh-profile-rdp-0").is_some(),
        "有可复用目标时应显示远程桌面图标"
    );
    assert!(
        cx.debug_bounds("ssh-profile-rdp-1").is_none(),
        "未记录远程桌面目标时不应显示入口"
    );
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
fn connection_manager_opens_recorded_remote_desktop_from_icon(cx: &mut TestAppContext) {
    let mut imported = profile();
    imported.origin = SshProfileOrigin::JumpServer;
    imported.remote_platform = RemotePlatformPreference::Windows;
    imported.rdp_web_enabled = Some(true);
    imported.jumpserver_rdp_session = Some(rdp_session(1, "production"));
    let (view, cx) = add_ssh_window(cx, service_with_profile_rdp(imported));
    cx.run_until_parked();
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    let button = cx
        .debug_bounds("ssh-profile-rdp-0")
        .expect("已记录的远程桌面图标应参与布局");
    cx.simulate_click(button.center(), Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://jump.example.com/lion/connect?token=00000000-0000-0000-0000-000000000002")
    );
}

#[gpui::test]
fn remote_session_panel_moves_entries_between_recent_and_favorites(cx: &mut TestAppContext) {
    let favorite = rdp_session(1, "favorite-windows");
    let recent = rdp_session(2, "recent-windows");
    let history = JumpServerRdpSessionHistory {
        favorites: vec![favorite.clone()],
        recent: vec![recent.clone()],
    };
    let (panel, cx) = add_remote_session_panel_window(cx, service_with_rdp_history(&history));
    cx.run_until_parked();
    cx.simulate_resize(size(px(820.0), px(560.0)));
    cx.run_until_parked();

    let favorites = cx
        .debug_bounds("remote-session-favorites")
        .expect("收藏列表应参与布局");
    let recent_list = cx
        .debug_bounds("remote-session-recent")
        .expect("最近会话列表应参与布局");
    assert!(
        favorites.origin.y < recent_list.origin.y,
        "收藏列表应排在最近会话之前"
    );
    assert!(cx.debug_bounds("remote-session-row-favorite-0").is_some());
    assert!(cx.debug_bounds("remote-session-row-recent-0").is_some());

    let favorite_button = cx
        .debug_bounds("remote-session-favorite-false-0")
        .expect("最近会话应提供收藏按钮");
    cx.simulate_click(favorite_button.center(), Modifiers::default());
    cx.run_until_parked();
    panel.read_with(cx, |panel, _| {
        assert_eq!(
            panel.history.favorites,
            vec![favorite.clone(), recent.clone()]
        );
        assert!(panel.history.recent.is_empty());
    });

    let unfavorite_button = cx
        .debug_bounds("remote-session-favorite-true-1")
        .expect("收藏会话应提供取消收藏按钮");
    cx.simulate_click(unfavorite_button.center(), Modifiers::default());
    cx.run_until_parked();
    panel.read_with(cx, |panel, _| {
        assert_eq!(panel.history.favorites, vec![favorite]);
        assert_eq!(panel.history.recent, vec![recent]);
    });
}

#[gpui::test]
fn jumpserver_panel_renders_login_assets_and_accounts(cx: &mut TestAppContext) {
    let (panel, cx) = add_jumpserver_panel_window(cx, service(Vec::new(), None));
    cx.run_until_parked();
    let asset = JumpServerAsset {
        id: "00000000-0000-0000-0000-000000000001".into(),
        org_id: "org-1".into(),
        name: "taiyuan-login".into(),
        address: "tycs.example.com".into(),
        platform: "Linux".into(),
        labels: vec![ramag_domain::entities::JumpServerLabel {
            name: "env".into(),
            value: "prod".into(),
        }],
        node_ids: vec!["node-industrial".into()],
        favorite: false,
        ungrouped: false,
        active: true,
    };
    let account = JumpServerAccount {
        id: "account-1".into(),
        alias: "account-1".into(),
        name: "root".into(),
        username: "root".into(),
        has_secret: true,
        can_connect: true,
    };
    panel.update(cx, |panel, cx| {
        let connection = JumpServerConnection::new(JumpServerCredential {
            base_url: "https://jump.example.com".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "password".into(),
        });
        panel.selected_connection_id = Some(connection.id.clone());
        panel.connections = Arc::new(vec![connection]);
        panel.session = Some(JumpServerSession {
            base_url: "https://jump.example.com/".into(),
            ssh_host: "jump.example.com".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "password".into(),
            token_keyword: "Bearer".into(),
            token: "token".into(),
            organizations: Vec::new(),
        });
        panel.assets = Arc::new(vec![asset.clone()]);
        panel.nodes = Arc::new(vec![
            ramag_domain::entities::JumpServerNode {
                id: "favorite".into(),
                org_id: "org-1".into(),
                key: "favorite".into(),
                name: "收藏夹".into(),
                full_name: "收藏夹".into(),
                assets_amount: 0,
            },
            ramag_domain::entities::JumpServerNode {
                id: "node-root".into(),
                org_id: "org-1".into(),
                key: "1".into(),
                name: "DEFAULT".into(),
                full_name: "DEFAULT".into(),
                assets_amount: 1,
            },
            ramag_domain::entities::JumpServerNode {
                id: "node-industrial".into(),
                org_id: "org-1".into(),
                key: "1:2".into(),
                name: "工业仿真".into(),
                full_name: "DEFAULT / 工业仿真".into(),
                assets_amount: 1,
            },
        ]);
        panel
            .expanded_tree_items
            .insert(super::jumpserver_dialog::tree_node_identity(
                "org-1",
                "node-root",
            ));
        panel.selected_tree_item = super::jumpserver_dialog::JumpServerTreeSelection::Node {
            org_id: "org-1".into(),
            node_id: "node-root".into(),
        };
        panel.selected_asset_id = Some(asset.id.clone());
        panel.selected_account_id = Some(account.id.clone());
        panel.detail = Some(JumpServerAssetDetail {
            asset,
            accounts: vec![account],
            ssh_enabled: true,
            rdp_web_enabled: true,
        });
        cx.notify();
    });
    cx.simulate_resize(size(px(920.0), px(820.0)));
    cx.run_until_parked();

    for selector in [
        "jumpserver-login-section",
        "jumpserver-source-section",
        "jumpserver-source-selector",
        "jumpserver-saved-connections",
        "new-jumpserver-connection",
        "jumpserver-assets-section",
        "jumpserver-asset-tree",
        "jumpserver-asset-table",
        "jumpserver-tree-node-0",
        "jumpserver-tree-node-1",
        "jumpserver-tree-node-2",
        "jumpserver-asset-row-0",
        "jumpserver-asset-action-0",
        "jumpserver-selected-detail",
        "jumpserver-inline-rdp-0",
        "jumpserver-inline-test-0",
        "jumpserver-inline-save-0",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} 应参与布局");
    }
    assert!(cx.debug_bounds("jumpserver-new-connection-form").is_none());
    assert!(cx.debug_bounds("jumpserver-url-field-input").is_none());
    let tree = cx
        .debug_bounds("jumpserver-asset-tree")
        .expect("asset tree should be rendered");
    let table = cx
        .debug_bounds("jumpserver-asset-table")
        .expect("asset table should be rendered");
    let bottom_delta =
        (tree.origin.y + tree.size.height - table.origin.y - table.size.height).abs();
    assert!(
        bottom_delta <= px(1.0),
        "资源树与资源表底边应对齐：{bottom_delta:?}"
    );
    let row = cx
        .debug_bounds("jumpserver-asset-row-0")
        .expect("resource row should be rendered");
    let action = cx
        .debug_bounds("jumpserver-asset-action-0")
        .expect("row action should be rendered");
    assert!(
        action.origin.y >= row.origin.y
            && action.origin.y + action.size.height <= row.origin.y + row.size.height,
        "操作应位于对应资源行内"
    );
    assert!(cx.debug_bounds("jumpserver-command-input").is_none());
}

#[gpui::test]
fn jumpserver_search_clear_restores_the_asset_list(cx: &mut TestAppContext) {
    let (panel, cx) = add_jumpserver_panel_window(cx, service(Vec::new(), None));
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        panel.session = Some(JumpServerSession {
            base_url: "https://jump.example.com/".into(),
            ssh_host: "jump.example.com".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "password".into(),
            token_keyword: "Bearer".into(),
            token: "token".into(),
            organizations: Vec::new(),
        });
        panel.assets = Arc::new(vec![
            JumpServerAsset {
                id: "00000000-0000-0000-0000-000000000001".into(),
                org_id: "org-1".into(),
                name: "linux-server".into(),
                address: "10.0.0.1".into(),
                platform: "Linux".into(),
                labels: Vec::new(),
                node_ids: Vec::new(),
                favorite: false,
                ungrouped: true,
                active: true,
            },
            JumpServerAsset {
                id: "00000000-0000-0000-0000-000000000002".into(),
                org_id: "org-1".into(),
                name: "windows-server".into(),
                address: "10.0.0.2".into(),
                platform: "Windows".into(),
                labels: Vec::new(),
                node_ids: Vec::new(),
                favorite: false,
                ungrouped: true,
                active: true,
            },
        ]);
        panel.operation = None;
        cx.notify();
    });
    cx.simulate_resize(size(px(920.0), px(820.0)));
    cx.run_until_parked();

    let search = cx
        .debug_bounds("jumpserver-asset-search")
        .expect("asset search should be rendered");
    cx.simulate_click(search.center(), Modifiers::default());
    cx.simulate_keystrokes("linux");
    cx.run_until_parked();
    panel.read_with(cx, |panel, _| {
        assert_eq!(panel.filtered_assets().len(), 1);
    });

    let clear = cx
        .debug_bounds("clear-jumpserver-asset-search")
        .expect("clear button should be rendered");
    cx.simulate_click(clear.center(), Modifiers::default());
    cx.run_until_parked();
    panel.read_with(cx, |panel, _| {
        assert!(panel.query.is_empty());
        assert_eq!(panel.filtered_assets().len(), 2);
    });
}

#[gpui::test]
fn jumpserver_rdp_button_opens_created_web_session(cx: &mut TestAppContext) {
    let (panel, cx) = add_jumpserver_panel_window(cx, service_with_jumpserver());
    cx.run_until_parked();
    let asset = JumpServerAsset {
        id: "00000000-0000-0000-0000-000000000001".into(),
        org_id: "org-1".into(),
        name: "windows".into(),
        address: "10.0.0.2".into(),
        platform: "Windows".into(),
        labels: Vec::new(),
        node_ids: Vec::new(),
        favorite: false,
        ungrouped: true,
        active: true,
    };
    let account = JumpServerAccount {
        id: "account-1".into(),
        alias: "account-1".into(),
        name: "admin".into(),
        username: "Administrator".into(),
        has_secret: true,
        can_connect: true,
    };
    panel.update(cx, |panel, cx| {
        let connection = JumpServerConnection::new(JumpServerCredential {
            base_url: "https://jump.example.com".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "password".into(),
        });
        panel.selected_connection_id = Some(connection.id.clone());
        panel.connections = Arc::new(vec![connection]);
        panel.session = Some(JumpServerSession {
            base_url: "https://jump.example.com/".into(),
            ssh_host: "jump.example.com".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "password".into(),
            token_keyword: "Bearer".into(),
            token: "api-token".into(),
            organizations: Vec::new(),
        });
        panel.assets = Arc::new(vec![asset.clone()]);
        panel.selected_asset_id = Some(asset.id.clone());
        panel.selected_account_id = Some(account.id.clone());
        panel.detail_error = Some("该资源未开放 SSH 协议，无法导入为 SSH 连接。".into());
        panel.detail = Some(JumpServerAssetDetail {
            asset,
            accounts: vec![account],
            ssh_enabled: false,
            rdp_web_enabled: true,
        });
        panel.operation = None;
        cx.notify();
    });
    cx.simulate_resize(size(px(920.0), px(820.0)));
    cx.run_until_parked();

    let button = cx
        .debug_bounds("jumpserver-inline-rdp-button-0")
        .expect("RDP 资产应显示远程桌面按钮");
    cx.simulate_click(button.center(), Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://jump.example.com/lion/connect?token=00000000-0000-0000-0000-000000000002")
    );
}

#[gpui::test]
fn jumpserver_new_connection_shows_form_test_and_save_actions(cx: &mut TestAppContext) {
    let (_panel, cx) = add_jumpserver_panel_window(cx, service(Vec::new(), None));
    cx.run_until_parked();
    cx.simulate_resize(size(px(920.0), px(720.0)));
    cx.run_until_parked();

    for selector in [
        "jumpserver-new-connection-form",
        "jumpserver-url-field-input",
        "jumpserver-password-field-input",
        "test-jumpserver-connection",
        "save-jumpserver-connection",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} 应参与布局");
    }
    assert!(cx.debug_bounds("load-jumpserver-assets").is_none());
}

#[gpui::test]
fn jumpserver_saved_connection_edit_reuses_connection_form(cx: &mut TestAppContext) {
    let (panel, cx) = add_jumpserver_panel_window(cx, service(Vec::new(), None));
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        let connection = JumpServerConnection::new(JumpServerCredential {
            base_url: "https://jump.example.com".into(),
            ssh_port: 2222,
            username: "alice".into(),
            password: "password".into(),
        });
        panel.selected_connection_id = Some(connection.id.clone());
        panel.connections = Arc::new(vec![connection]);
        panel.editing_connection = true;
        cx.notify();
    });
    cx.simulate_resize(size(px(920.0), px(720.0)));
    cx.run_until_parked();

    for selector in [
        "jumpserver-new-connection-form",
        "test-jumpserver-connection",
        "save-jumpserver-connection",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} 应参与布局");
    }
}

#[gpui::test]
fn jumpserver_catalog_defaults_to_organization_with_assets(cx: &mut TestAppContext) {
    let (panel, cx) = add_jumpserver_panel_window(cx, service(Vec::new(), None));
    cx.run_until_parked();
    panel.update(cx, |panel, _| {
        panel.apply_catalog(JumpServerCatalog {
            assets: vec![JumpServerAsset {
                id: "00000000-0000-0000-0000-000000000001".into(),
                org_id: "org-default".into(),
                name: "server".into(),
                address: "10.0.0.1".into(),
                platform: "Linux".into(),
                labels: Vec::new(),
                node_ids: vec!["node-default".into()],
                favorite: false,
                ungrouped: false,
                active: true,
            }],
            nodes: vec![
                ramag_domain::entities::JumpServerNode {
                    id: "node-empty".into(),
                    org_id: "org-all".into(),
                    key: "1".into(),
                    name: "All Organizations".into(),
                    full_name: "All Organizations".into(),
                    assets_amount: 0,
                },
                ramag_domain::entities::JumpServerNode {
                    id: "node-default".into(),
                    org_id: "org-default".into(),
                    key: "1".into(),
                    name: "DEFAULT".into(),
                    full_name: "DEFAULT".into(),
                    assets_amount: 1,
                },
            ],
        });
    });

    panel.read_with(cx, |panel, _| {
        assert_eq!(panel.filtered_assets().len(), 1);
        assert_eq!(
            panel.selected_tree_item,
            super::jumpserver_dialog::JumpServerTreeSelection::Node {
                org_id: "org-default".into(),
                node_id: "node-default".into(),
            }
        );
    });
}

#[test]
fn jumpserver_unavailable_account_message_explains_connect_permission() {
    let detail = JumpServerAssetDetail {
        asset: JumpServerAsset {
            id: "00000000-0000-0000-0000-000000000001".into(),
            org_id: "org-1".into(),
            name: "server".into(),
            address: "10.0.0.1".into(),
            platform: "Linux".into(),
            labels: Vec::new(),
            node_ids: Vec::new(),
            favorite: false,
            ungrouped: false,
            active: true,
        },
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "root".into(),
            username: "root".into(),
            has_secret: true,
            can_connect: false,
        }],
        ssh_enabled: true,
        rdp_web_enabled: false,
    };

    let message = super::jumpserver_dialog::detail_unavailable_message(&detail)
        .expect("unavailable detail should explain the reason");
    assert!(message.contains("connect 权限"));
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
        cx.debug_bounds("ssh-command-input").is_some(),
        "新增连接应提供 SSH 命令解析入口"
    );
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
fn edit_profile_form_keeps_fields_and_ssh_command_parser(cx: &mut TestAppContext) {
    let (form, cx) =
        add_ssh_form_window_with_profile(cx, service(Vec::new(), None), Some(profile()));
    cx.simulate_resize(size(px(720.0), px(800.0)));
    cx.run_until_parked();

    form.read_with(cx, |form, _| assert_eq!(form.title(), "编辑"));
    assert!(
        cx.debug_bounds("ssh-profile-host-field-input").is_some(),
        "编辑连接应保留标准 SSH 字段"
    );
    assert!(
        cx.debug_bounds("ssh-command-input").is_some(),
        "编辑连接也应提供 SSH 命令解析入口"
    );
}

#[gpui::test]
fn windows_workspace_lists_accessible_drives_before_the_home_directory(cx: &mut TestAppContext) {
    let mut profile = SshProfile::new("windows", "windows.example");
    profile.username = "Administrator".into();
    profile.remote_platform = RemotePlatformPreference::Windows;
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile.id.clone(),
            last_remote_path: ".".into(),
        }],
        active_profile_id: Some(profile.id.clone()),
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(cx, service(vec![profile.clone()], Some(preference)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        let workspace = view
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &profile.id)
            .expect("Windows workspace should be restored");
        assert_eq!(workspace.path, "/");
        assert_eq!(
            workspace
                .entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.path.as_str()))
                .collect::<Vec<_>>(),
            [("C:", "/C:/"), ("D:", "/D:/")]
        );
        assert!(workspace.sftp_error.is_none());
        assert_eq!(
            workspace
                .capabilities
                .as_ref()
                .map(|capabilities| capabilities.sftp_namespace),
            Some(SftpNamespaceKind::Virtual)
        );
        assert_eq!(
            workspace
                .capabilities
                .as_ref()
                .map(|capabilities| capabilities.shell),
            Some(RemoteShellKind::Cmd)
        );
    });
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            let drive = view
                .workspaces
                .iter()
                .find(|workspace| workspace.profile_id() == &profile.id)
                .and_then(|workspace| workspace.entries.first())
                .cloned()
                .expect("Windows drive should be rendered");
            view.activate_remote_entry(profile.id.clone(), drive, window, cx);
        });
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        let workspace = view
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &profile.id)
            .expect("Windows workspace should remain open");
        assert_eq!(workspace.path, "/C:/");
        assert!(workspace.sftp_error.is_none());
    });
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
        path_favorites: Vec::new(),
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
            let resize = view
                .workspace_resizes
                .get(&profile.id)
                .cloned()
                .expect("活动工作区应有独立分栏状态");
            resize.update(cx, |state, cx| {
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
    assert!(
        cx.debug_bounds("ssh-terminal-drop-target").is_some(),
        "终端区域应接收远程目录拖放"
    );
    let directory = cx
        .debug_bounds("sftp-entry-1")
        .expect("directory entry should be rendered");
    let terminal_target = cx
        .debug_bounds("ssh-terminal-drop-target")
        .expect("terminal drop target should be rendered");
    let generation_before = view.read_with(cx, |view, _| view.workspaces[0].terminal_generation);
    let drag_start = point(
        directory.origin.x + px(12.0),
        directory.origin.y + directory.size.height / 2.0,
    );
    let drop_point = point(
        terminal_target.origin.x + terminal_target.size.width / 2.0,
        terminal_target.origin.y + terminal_target.size.height / 2.0,
    );
    cx.simulate_mouse_down(drag_start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(drag_start.x + px(12.0), drag_start.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(drop_point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(drop_point, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(
            view.workspaces[0].terminal_generation > generation_before,
            "拖入目录后应启动一个新终端，不能复用当前终端"
        );
    });
    let file = cx
        .debug_bounds("sftp-entry-0")
        .expect("file entry should be rendered");
    let file_generation_before =
        view.read_with(cx, |view, _| view.workspaces[0].terminal_generation);
    let file_drag_start = point(
        file.origin.x + px(12.0),
        file.origin.y + file.size.height / 2.0,
    );
    cx.simulate_mouse_down(file_drag_start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(file_drag_start.x + px(12.0), file_drag_start.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(drop_point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(drop_point, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.workspaces[0].terminal_generation, file_generation_before,
            "文件行不应伪装成目录拖动入口"
        );
    });
    let path_label = cx
        .debug_bounds("ssh-directory-path-label")
        .expect("directory path label should be rendered");
    let path_generation_before =
        view.read_with(cx, |view, _| view.workspaces[0].terminal_generation);
    let path_drag_start = point(
        path_label.origin.x + path_label.size.width / 2.0,
        path_label.origin.y + path_label.size.height / 2.0,
    );
    cx.simulate_mouse_down(path_drag_start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(path_drag_start.x - px(12.0), path_drag_start.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(drop_point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(drop_point, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(
            view.workspaces[0].terminal_generation > path_generation_before,
            "拖入当前路径后应在该目录启动一个新终端"
        );
    });
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
fn production_workspace_renders_safe_diagnostics_without_terminal(cx: &mut TestAppContext) {
    let mut profile = profile();
    profile.production = true;
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile.id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile.id.clone()),
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(cx, service(vec![profile.clone()], Some(preference)));
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    assert!(cx.debug_bounds("ssh-safe-diagnostic-pane").is_some());
    assert!(cx.debug_bounds("ssh-terminal-drop-target").is_none());
    view.read_with(cx, |view, _| {
        let workspace = view
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &profile.id)
            .expect("production workspace should exist");
        assert!(workspace.terminals.is_empty());
    });
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
        path_favorites: Vec::new(),
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

#[gpui::test]
fn workspace_resize_is_isolated_by_connection(cx: &mut TestAppContext) {
    let first = profile();
    let second = SshProfile::new("staging", "staging.example");
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
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(
        cx,
        service(vec![first.clone(), second.clone()], Some(preference)),
    );
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            let resize = view
                .workspace_resizes
                .get(&first.id)
                .cloned()
                .expect("首个连接应有独立分栏状态");
            resize.update(cx, |state, cx| {
                state.resize_panel(0, px(360.0), window, cx);
            });
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("ssh-file-browser")
            .expect("首个连接应显示文件栏")
            .size
            .width,
        px(360.0)
    );

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.select_workspace(second.id.clone(), window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("ssh-file-browser")
            .expect("第二个连接应显示文件栏")
            .size
            .width,
        px(280.0),
        "第二个连接不应继承首个连接拖动后的宽度"
    );

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.select_workspace(first.id.clone(), window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("ssh-file-browser")
            .expect("切回首个连接应显示文件栏")
            .size
            .width,
        px(360.0),
        "切回首个连接应保留当前会话内自己的宽度"
    );
}

#[gpui::test]
fn close_shortcut_closes_first_workspace_when_no_terminal_exists(cx: &mut TestAppContext) {
    let profile = profile();
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile.id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile.id.clone()),
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(cx, service(vec![profile], Some(preference)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert_eq!(view.workspaces.len(), 1);
        assert!(view.workspaces[0].terminals.is_empty());
    });
    cx.update(|window, app| {
        let focus = view.read(app).focus_handle.clone();
        window.focus(&focus, app);
        window.dispatch_action(Box::new(crate::CloseSshTerminal), app);
    });
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert!(view.workspaces.is_empty());
        assert_eq!(view.active_workspace_id, None);
        assert_eq!(view.view_mode, ViewMode::Manager);
    });
}

#[cfg(unix)]
#[gpui::test]
fn close_shortcut_selects_and_focuses_previous_terminal(cx: &mut TestAppContext) {
    let profile = profile();
    let profile_id = profile.id.clone();
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile_id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile_id.clone()),
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(cx, service(vec![profile], Some(preference)));
    cx.run_until_parked();

    let mut previous_terminal = None;
    let mut active_terminal = None;
    cx.update(|window, app| {
        let terminals = (1..=3)
            .map(|id| {
                let core = TerminalCore::start(TerminalCommand::new("/bin/sh", Vec::new()))
                    .expect("测试终端应启动");
                let terminal = app.new(|cx| TerminalView::new(core, window, cx));
                TerminalTab {
                    id,
                    label: format!("终端 {id}").into(),
                    view: terminal,
                }
            })
            .collect::<Vec<_>>();
        previous_terminal = Some(terminals[1].view.clone());
        active_terminal = Some(terminals[2].view.clone());
        view.update(app, |view, cx| {
            let workspace = view
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.profile_id() == &profile_id)
                .expect("工作区应存在");
            workspace.terminals = terminals;
            workspace.active_terminal_id = Some(3);
            cx.notify();
        });
    });
    cx.run_until_parked();

    cx.update(|window, app| {
        active_terminal
            .as_ref()
            .expect("当前终端应存在")
            .read(app)
            .focus_handle(app)
            .focus(window, app);
        window.dispatch_action(Box::new(crate::CloseSshTerminal), app);
    });
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        let workspace = view
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &profile_id)
            .expect("工作区应存在");
        assert_eq!(workspace.active_terminal_id, Some(2));
        assert_eq!(
            workspace
                .terminals
                .iter()
                .map(|terminal| terminal.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    });
    assert!(cx.update(|window, app| {
        previous_terminal
            .as_ref()
            .expect("上一个终端应存在")
            .read(app)
            .focus_handle(app)
            .is_focused(window)
    }));
}

#[cfg(unix)]
#[gpui::test]
fn reconnect_replaces_the_current_terminal_without_creating_a_tab(cx: &mut TestAppContext) {
    let mut profile = profile();
    profile.production = false;
    let profile_id = profile.id.clone();
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile_id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile_id.clone()),
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(cx, service_with_working_terminal(profile, preference));
    cx.run_until_parked();

    cx.update(|window, app| {
        let core = TerminalCore::start(TerminalCommand::new(
            "/bin/sh",
            vec!["-c".into(), "exit 7".into()],
        ))
        .expect("测试终端应启动");
        let terminal = app.new(|cx| TerminalView::new(core, window, cx));
        view.update(app, |view, cx| {
            let workspace = view
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.profile_id() == &profile_id)
                .expect("工作区应存在");
            workspace.terminals = vec![TerminalTab {
                id: 41,
                label: "终端 9".into(),
                view: terminal,
            }];
            workspace.active_terminal_id = Some(41);
            workspace.next_terminal_ordinal = 10;
            cx.notify();
        });
    });
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.reconnect_terminal(profile_id.clone(), 41, window, cx);
        });
    });
    cx.run_until_parked();

    let reconnected_terminal = view.read_with(cx, |view, _| {
        let workspace = view
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &profile_id)
            .expect("工作区应存在");
        assert_eq!(workspace.terminals.len(), 1);
        assert_eq!(workspace.terminals[0].id, 41);
        assert_eq!(workspace.terminals[0].label.as_ref(), "终端 9");
        assert_eq!(workspace.active_terminal_id, Some(41));
        assert_eq!(workspace.next_terminal_ordinal, 10);
        workspace.terminals[0].view.clone()
    });
    let exit_code = cx.update(|_, app| {
        reconnected_terminal
            .read(app)
            .core()
            .exit_status()
            .and_then(|status| status.code)
    });
    assert_eq!(exit_code, Some(0));
}

#[gpui::test]
fn empty_state_close_button_closes_first_workspace(cx: &mut TestAppContext) {
    let profile = profile();
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile.id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile.id.clone()),
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(cx, service(vec![profile], Some(preference)));
    cx.run_until_parked();

    let close = cx
        .debug_bounds("close-empty-ssh-workspace")
        .expect("空终端工作区应提供关闭连接按钮");
    let close_point = point(
        close.origin.x + close.size.width / 2.0,
        close.origin.y + close.size.height / 2.0,
    );
    cx.simulate_mouse_down(close_point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(close_point, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert!(view.workspaces.is_empty());
        assert_eq!(view.active_workspace_id, None);
        assert_eq!(view.view_mode, ViewMode::Manager);
    });
}

#[gpui::test]
fn restored_workspace_keeps_favorites_per_profile(cx: &mut TestAppContext) {
    let profile = profile();
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile.id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile.id.clone()),
        path_favorites: vec![SshPathFavorites {
            profile_id: profile.id.clone(),
            paths: vec!["/var/log".into()],
        }],
    };
    let (view, cx) = add_ssh_window(cx, service(vec![profile.clone()], Some(preference)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert_eq!(
            view.path_favorites.get(&profile.id),
            Some(&vec!["/var/log".into()])
        );
    });
}
