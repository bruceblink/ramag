use super::*;

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

    let message = crate::views::jumpserver_dialog::detail_unavailable_message(&detail)
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
fn production_workspace_renders_terminal_warning_and_hides_sftp_writes(cx: &mut TestAppContext) {
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

    assert!(cx.debug_bounds("ssh-production-terminal-warning").is_some());
    assert!(cx.debug_bounds("ssh-terminal-drop-target").is_some());
    assert!(cx.debug_bounds("sftp-upload").is_none());
    assert!(cx.debug_bounds("sftp-mkdir").is_none());
    view.read_with(cx, |view, _| {
        let workspace = view
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &profile.id)
            .expect("production workspace should exist");
        assert!(!workspace.terminal_loading);
    });
}
