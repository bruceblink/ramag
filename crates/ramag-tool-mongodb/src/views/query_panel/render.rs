use super::*;

impl Render for MongoQueryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let border = theme.border;

        let only_one = self.tabs.len() <= 1;
        let can_add_tab = can_open_editor_tab(self.tabs.len());
        let add_tab_disabled = self.connection.is_none() || !can_add_tab;
        let tab_items: Vec<gpui::AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, _tab)| {
                let title = self.titles.get(i).cloned().unwrap_or_default();
                let is_active = i == self.active;
                let row = h_flex()
                    .id(SharedString::from(format!("mongo-tab-{i}")))
                    .px(px(10.0))
                    .h(px(28.0))
                    .gap(px(6.0))
                    .flex_none()
                    .items_center()
                    .border_r_1()
                    .border_color(border)
                    .text_xs()
                    .when(is_active, |s| {
                        s.bg(theme.background)
                            .text_color(fg)
                            .border_b_1()
                            .border_color(theme.primary)
                    })
                    .when(!is_active, |s| s.text_color(muted))
                    .hover(|s| s.bg(theme.list_hover))
                    .cursor_pointer()
                    .child(SharedString::from(title))
                    .when(!only_one, |tab| {
                        tab.child(
                            // 父标签在 mouse_down 时切换；关闭按钮必须更早阻止冒泡。
                            div()
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(
                                    ramag_ui::clickable_button(SharedString::from(format!(
                                        "mongo-tab-close-{i}"
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip("关闭")
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.close_tab(i, window, cx);
                                        },
                                    )),
                                ),
                        )
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.select_tab(i, window, cx)),
                    );
                row.into_any_element()
            })
            .collect();

        let body: gpui::AnyElement = if let Some(tab) = self.tabs.get(self.active) {
            tab.clone().into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(
                    "（左侧选 collection 自动开 Tab，或点 + 新 Tab）",
                ))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_w_0()
            .track_focus(&self.focus_handle)
            .bg(theme.background)
            .key_context("MongoQueryPanel")
            .on_action(
                cx.listener(|this, _: &NewMongoQueryTab, window, cx| {
                    this.add_tab(window, cx);
                }),
            )
            // 草稿落盘失败常驻警示：用户以为可跨重启恢复，静默失败等于丢稿
            .when_some(self.draft_persist_error.clone(), |panel, err| {
                let warning = theme.warning;
                let mut warn_bg = warning;
                warn_bg.a = 0.12;
                panel.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_none()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py(px(5.0))
                        .bg(warn_bg)
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(warning)
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(format!(
                                    "⚠ 草稿自动保存失败：{err}（草稿可能无法跨重启恢复，请复制重要内容备份）"
                                ))),
                        )
                        .child(
                            ramag_ui::clickable_button("mongo-draft-persist-retry")
                                .ghost()
                                .small()
                                .label("重试")
                                .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                                    this.schedule_draft_persist(cx);
                                })),
                        ),
                )
            })
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if this.tab_count() > 0 {
                    let i = this.active;
                    this.close_tab(i, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleMongoEditor, window, cx| {
                this.toggle_editor(window, cx);
            }))
            .when(self.show_editor, |panel| {
                panel.child(
                    h_flex()
                        .debug_selector(|| "mongo-editor-toolbar".into())
                        .w_full()
                        .min_w_0()
                        .flex_none()
                        .min_h(px(32.0))
                        .flex_wrap()
                        .items_center()
                        .border_b_1()
                        .border_color(border)
                        .bg(theme.muted.opacity(0.10))
                        .child(
                            h_flex()
                                .id("mongo-tabs-scroll")
                                .flex_1()
                                .min_w_0()
                                .items_center()
                                .overflow_x_scroll()
                                .track_scroll(&self.tabs_scroll)
                                .children(tab_items)
                                .child(
                                    ramag_ui::clickable_button("mongo-tab-add")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Plus)
                                        .when(add_tab_disabled, |button| {
                                            button.tooltip(if self.connection.is_none() {
                                                "请先连接".to_string()
                                            } else {
                                                format!("最多 {MAX_EDITOR_TABS} 个标签")
                                            })
                                        })
                                        .disabled(add_tab_disabled)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.add_tab(window, cx);
                                            },
                                        )),
                                ),
                        )
                        .child(
                            h_flex()
                                .debug_selector(|| "mongo-editor-actions".into())
                                .min_w_0()
                                .flex_none()
                                .items_center()
                                .border_l_1()
                                .border_color(border)
                                .child(
                                    ramag_ui::clickable_button("mongo-history")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Calendar)
                                        .tooltip("历史")
                                        .disabled(self.connection.is_none())
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.open_history_dialog(window, cx);
                                            },
                                        )),
                                )
                                .child({
                                    let entity = cx.entity();
                                    let coll = self
                                        .tabs
                                        .get(self.active)
                                        .and_then(|t| t.read(cx).collection.clone())
                                        .unwrap_or_default();
                                    ramag_ui::clickable_button("mongo-examples")
                                        .ghost()
                                        .small()
                                        .icon(icons::scroll_text())
                                        .tooltip("示例")
                                        .pointer_dropdown_menu(move |menu, _, _| {
                                            let mut m = menu;
                                            for (label, cmd) in
                                                crate::views::examples::mongo_examples(&coll)
                                            {
                                                let e = entity.clone();
                                                m = m.item(ramag_ui::menu_item(label).on_click(
                                                    move |_, window, app| {
                                                        e.update(app, |panel, cx| {
                                                            panel.apply_example(&cmd, window, cx);
                                                        });
                                                    },
                                                ));
                                            }
                                            m
                                        })
                                })
                                .child(
                                    ramag_ui::clickable_button("mongo-format")
                                        .ghost()
                                        .small()
                                        .icon(icons::wand_sparkles())
                                        .tooltip("格式化")
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                if let Some(tab) =
                                                    this.tabs.get(this.active).cloned()
                                                {
                                                    tab.update(cx, |t, cx| {
                                                        t.format_json(window, cx)
                                                    });
                                                }
                                            },
                                        )),
                                ),
                        ),
                )
            })
            .child(div().flex_1().min_w_0().min_h_0().child(body))
    }
}
