//! JumpServer 界面测试。

use super::*;

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
        cx.debug_bounds("ssh-profile-rdp-copy-0").is_some(),
        "有可复用目标时应显示一次性链接复制图标"
    );
    assert!(
        cx.debug_bounds("ssh-profile-rdp-1").is_none(),
        "未记录远程桌面目标时不应显示入口"
    );
    assert!(
        cx.debug_bounds("ssh-profile-rdp-copy-1").is_none(),
        "未记录远程桌面目标时不应显示一次性链接复制入口"
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
fn connection_manager_copies_fresh_remote_desktop_link_from_icon(cx: &mut TestAppContext) {
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
        .debug_bounds("ssh-profile-rdp-copy-0")
        .expect("一次性链接复制图标应参与布局");
    cx.simulate_click(button.center(), Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(
            "https://jump.example.com/lion/connect?token=00000000-0000-0000-0000-000000000002"
                .into()
        )
    );
    assert_eq!(cx.opened_url(), None, "复制链接不应自动打开浏览器");
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
            .insert(crate::views::jumpserver_dialog::tree_node_identity(
                "org-1",
                "node-root",
            ));
        panel.selected_tree_item = crate::views::jumpserver_dialog::JumpServerTreeSelection::Node {
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
            crate::views::jumpserver_dialog::JumpServerTreeSelection::Node {
                org_id: "org-default".into(),
                node_id: "node-default".into(),
            }
        );
    });
}
