use super::*;

impl SshView {
    pub(super) fn render_file_browser(
        &self,
        workspace_id: SshProfileId,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &workspace_id)
        else {
            return div().into_any_element();
        };
        let path = workspace.path.clone();
        let entries = workspace.entries.clone();
        let filtered_indices = filtered_entry_indices(&entries, &workspace.directory_query);
        let visible_len = filtered_indices
            .as_ref()
            .map_or(entries.len(), |indices| indices.len());
        let selected_path = workspace.selected_path.clone();
        let loading = workspace.sftp_loading;
        let loading_path = workspace.directory_loading_path.clone();
        let error = workspace.sftp_error.clone();
        let empty_directory_message = empty_directory_message(workspace);
        let busy = workspace.operation_busy;
        let preview_loading = workspace.file_preview_loading;
        let sftp_locked = workspace.profile.production;
        let connection_available = self.profile_connection_available(&workspace.profile);
        let (total_directories, total_files) = directory_counts(&entries);
        let (visible_directories, visible_files) = filtered_indices
            .as_ref()
            .map_or((total_directories, total_files), |indices| {
                directory_counts_at(&entries, indices)
            });
        let mut summary = if filtered_indices.is_some() {
            format!(
                "目录 {visible_directories}/{total_directories} · 文件 {visible_files}/{total_files}"
            )
        } else {
            format!("目录 {total_directories} · 文件 {total_files}")
        };
        if let Some(transport) = workspace
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.sftp_transport)
        {
            summary.push_str(match transport {
                SftpTransportKind::StandardSubsystem => " · 标准 SFTP",
                SftpTransportKind::WindowsCompatibility => " · Windows 兼容 SFTP",
            });
        }
        if sftp_locked {
            summary.push_str(" · 只读");
        }
        let has_transfers = self
            .service
            .transfer_tasks()
            .iter()
            .any(|task| task.profile_id == workspace_id);
        let transfers_visible = workspace.transfers_visible;
        let border = cx.theme().border;

        let show_transfers = has_transfers;
        let toolbar = ramag_ui::responsive_toolbar()
            .debug_selector(|| "ssh-directory-toolbar".into())
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .px(px(6.0))
            .py(px(6.0))
            .bg(cx.theme().secondary)
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .id("ssh-directory-search")
                    .debug_selector(|| "ssh-directory-search".into())
                    .flex_1()
                    .min_w(px(96.0))
                    .child(
                        ramag_ui::cleanable_input(
                            &self.directory_search,
                            "ssh-directory-search-clear",
                            false,
                            cx,
                        )
                        .small()
                        .prefix(
                            Icon::new(IconName::Search)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        ),
                    ),
            )
            .child(
                ramag_ui::clickable_button("sftp-refresh")
                    .ghost()
                    .xsmall()
                    .flex_none()
                    .debug_selector(|| "sftp-refresh".into())
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("刷新")
                    .disabled(loading || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.refresh_active_directory(cx);
                    })),
            )
            .when(!sftp_locked, |toolbar| {
                toolbar
                    .child(
                        ramag_ui::clickable_button("sftp-upload")
                            .ghost()
                            .xsmall()
                            .flex_none()
                            .debug_selector(|| "sftp-upload".into())
                            .icon(ramag_ui::icons::upload())
                            .tooltip("上传")
                            .disabled(loading || busy || !connection_available)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.pick_upload(window, cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("sftp-mkdir")
                            .ghost()
                            .xsmall()
                            .flex_none()
                            .debug_selector(|| "sftp-mkdir".into())
                            .icon(ramag_ui::icons::folder_plus())
                            .tooltip("新建")
                            .disabled(loading || busy || !connection_available)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.prompt_create_directory(window, cx);
                            })),
                    )
            })
            .when(show_transfers, |toolbar| {
                toolbar.child(
                    div()
                        .id("ssh-directory-transfers")
                        .debug_selector(|| "ssh-directory-transfers".into())
                        .child(
                            ramag_ui::clickable_button("ssh-directory-transfers-button")
                                .ghost()
                                .xsmall()
                                .icon(ramag_ui::icons::arrow_up_down())
                                .tooltip("传输")
                                .selected(transfers_visible)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.toggle_transfer_panel(cx);
                                })),
                        ),
                )
            });

        let body: AnyElement = if !connection_available && entries.is_empty() {
            centered_message("OpenSSH 不可用，请编辑连接", cx).into_any_element()
        } else if loading && entries.is_empty() {
            centered_message("加载中…", cx).into_any_element()
        } else if let Some(error) = error {
            let workspace_for_direct = workspace_id.clone();
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .px(px(14.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("加载失败：{error}")),
                )
                .child(
                    h_flex()
                        .gap(px(8.0))
                        .child(
                            ramag_ui::clickable_button("retry-sftp")
                                .small()
                                .label("重试")
                                .disabled(!connection_available)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.refresh_active_directory(cx);
                                })),
                        )
                        .child(
                            div()
                                .id("ssh-directory-direct")
                                .debug_selector(|| "ssh-directory-direct".into())
                                .child(
                                    ramag_ui::clickable_button("open-sftp-path")
                                        .small()
                                        .label("直达")
                                        .disabled(!connection_available)
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                this.prompt_remote_path(
                                                    workspace_for_direct.clone(),
                                                    window,
                                                    cx,
                                                );
                                            },
                                        )),
                                ),
                        ),
                )
                .into_any_element()
        } else if entries.is_empty() {
            centered_message(empty_directory_message, cx).into_any_element()
        } else if visible_len == 0 {
            centered_message("暂无匹配", cx).into_any_element()
        } else {
            let menu_state = RemoteEntryMenuState {
                connection_available,
                allow_write: !sftp_locked,
                directory_loading: loading,
                operation_busy: busy,
                preview_loading,
            };
            uniform_list(
                SharedString::from(format!("sftp-directory-{workspace_id}")),
                visible_len,
                cx.processor({
                    let entries = entries.clone();
                    let filtered_indices = filtered_indices.clone();
                    let selected_path = selected_path.clone();
                    let loading_path = loading_path.clone();
                    let workspace_id = workspace_id.clone();
                    move |_this, range: Range<usize>, _window, cx| {
                        range
                            .map(|row_index| {
                                let index = filtered_indices
                                    .as_ref()
                                    .map_or(row_index, |indices| indices[row_index]);
                                remote_entry_row(
                                    entries[index].clone(),
                                    selected_path.as_ref(),
                                    workspace_id.clone(),
                                    index,
                                    loading
                                        && loading_path.as_deref()
                                            == Some(entries[index].path.as_str()),
                                    menu_state,
                                    cx,
                                )
                            })
                            .collect::<Vec<_>>()
                    }
                }),
            )
            .size_full()
            .into_any_element()
        };

        v_flex()
            .id("ssh-file-browser")
            .debug_selector(|| "ssh-file-browser".into())
            .size_full()
            .border_r_1()
            .border_color(border)
            .child(self.render_directory_breadcrumb(workspace_id.clone(), &path, cx))
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
            .child(
                h_flex()
                    .id("ssh-directory-summary")
                    .debug_selector(|| "ssh-directory-summary".into())
                    .w_full()
                    .h(px(32.0))
                    .flex_none()
                    .items_center()
                    .px(px(10.0))
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary),
            )
            .into_any_element()
    }
}
