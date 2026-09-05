use super::*;

fn assert_inside(
    parent: gpui::Bounds<gpui::Pixels>,
    child: gpui::Bounds<gpui::Pixels>,
    label: &str,
) {
    assert!(
        child.origin.x >= parent.origin.x
            && child.origin.y >= parent.origin.y
            && child.right() <= parent.right()
            && child.bottom() <= parent.bottom(),
        "{label} 越出父容器：parent={parent:?}, child={child:?}"
    );
}

#[gpui::test]
fn directory_toolbar_wraps_controls_inside_supported_file_browser_widths(cx: &mut TestAppContext) {
    let profile = profile();
    let preference = SshWorkspacePreference {
        workspaces: vec![SshWorkspaceState {
            profile_id: profile.id.clone(),
            last_remote_path: "/home/alice".into(),
        }],
        active_profile_id: Some(profile.id.clone()),
        path_favorites: Vec::new(),
    };
    let (view, cx) = add_ssh_window(cx, service(vec![profile.clone()], Some(preference)));
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
        workspace.directory_loaded = true;
        workspace.sftp_loading = false;
        workspace.directory_loading_path = None;
        cx.notify();
    });

    for (viewport_width, browser_width) in [(360.0, 180.0), (800.0, 280.0), (1440.0, 600.0)] {
        cx.simulate_resize(size(px(viewport_width), px(720.0)));
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                let resize = view
                    .workspace_resizes
                    .get(&profile.id)
                    .cloned()
                    .expect("workspace should have a resize state");
                resize.update(cx, |state, cx| {
                    state.resize_panel(0, px(browser_width), window, cx);
                });
            });
        });
        cx.run_until_parked();

        let file_browser = cx
            .debug_bounds("ssh-file-browser")
            .expect("SSH 文件浏览器应渲染");
        let toolbar = cx
            .debug_bounds("ssh-directory-toolbar")
            .expect("SSH 目录工具栏应渲染");
        let search = cx
            .debug_bounds("ssh-directory-search")
            .expect("SSH 目录搜索框应渲染");
        assert_inside(file_browser, toolbar, "SSH 目录工具栏");
        assert_inside(toolbar, search, "SSH 目录搜索框");

        for selector in ["sftp-refresh", "sftp-upload", "sftp-mkdir"] {
            let button = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} 应渲染"));
            assert_inside(toolbar, button, selector);
        }

        if browser_width == 180.0 {
            let refresh = cx.debug_bounds("sftp-refresh").expect("刷新按钮应渲染");
            assert!(
                refresh.origin.y > search.origin.y,
                "最小文件栏宽度应让操作按钮换到搜索框下方：search={search:?}, refresh={refresh:?}"
            );
        }
    }
}
