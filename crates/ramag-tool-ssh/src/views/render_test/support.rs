//! SSH 渲染测试夹具。

use super::*;

pub(super) fn profile() -> SshProfile {
    let mut profile = SshProfile::new("production", "server.example");
    profile.username = "alice".into();
    profile.initial_directory = Some("/home/alice".into());
    profile
}

pub(super) fn service(
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
pub(super) fn service_with_working_terminal(
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

pub(super) fn service_with_jumpserver() -> Arc<SshService> {
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

pub(super) fn service_with_profile_rdp(profile: SshProfile) -> Arc<SshService> {
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

pub(super) fn service_with_rdp_history(history: &JumpServerRdpSessionHistory) -> Arc<SshService> {
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

pub(super) fn rdp_session(index: u32, asset_name: &str) -> JumpServerRdpSession {
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

pub(super) fn add_ssh_window(
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

pub(super) fn add_ssh_form_window(
    cx: &mut TestAppContext,
    service: Arc<SshService>,
) -> (Entity<SshProfileFormPanel>, &mut VisualTestContext) {
    add_ssh_form_window_with_profile(cx, service, None)
}

pub(super) fn add_ssh_form_window_with_profile(
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

pub(super) fn add_jumpserver_panel_window(
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

pub(super) fn add_remote_session_panel_window(
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
