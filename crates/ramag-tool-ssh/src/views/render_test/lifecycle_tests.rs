//! SSH 工作区生命周期测试。

use super::*;

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
