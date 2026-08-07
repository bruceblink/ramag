use super::*;

impl Render for QueryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let secondary_bg = theme.secondary;
        let muted_bg = theme.muted;
        let accent = theme.accent;

        let active = self.active;
        let titles: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let dt = t.read(cx).display_title().to_string();
                if dt.is_empty() {
                    self.titles.get(i).cloned().unwrap_or_default()
                } else {
                    dt
                }
            })
            .collect();
        let only_one = titles.len() <= 1;
        let can_add_tab = can_open_editor_tab(self.tabs.len());

        let current_view: Option<AnyView> = self.tabs.get(active).map(|t| t.clone().into());

        let tab_bar_items: Vec<gpui::AnyElement> = titles
            .iter()
            .enumerate()
            .map(|(idx, title)| {
                let is_active = idx == active;
                let title = title.clone();
                let id_select = SharedString::from(format!("tab-{idx}"));
                let id_close = SharedString::from(format!("tab-close-{idx}"));

                let mut tab = h_flex()
                    .id(id_select)
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(7.0))
                    .border_r_1()
                    .border_color(border)
                    .cursor_pointer()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if is_active { fg } else { muted_fg })
                            .child(title),
                    )
                    .when(!only_one, |tab| {
                        tab.child(
                            ramag_ui::clickable_button(id_close)
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(idx, window, cx);
                                })),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_tab(idx, window, cx);
                    }));

                if is_active {
                    tab = tab.bg(theme_active_bg(secondary_bg, accent));
                } else {
                    tab = tab.hover(move |this| this.bg(muted_bg));
                }

                tab.into_any_element()
            })
            .collect();

        v_flex()
            .size_full()
            .key_context("QueryPanel")
            .on_action(cx.listener(|this, _: &NewQueryTab, window, cx| {
                this.add_tab(window, cx);
            }))
            // 草稿落盘失败常驻警示：用户以为可跨重启恢复，静默失败等于丢稿
            .when_some(self.draft_persist_error.clone(), |panel, err| {
                let warning = theme.warning;
                let mut warn_bg = warning;
                warn_bg.a = 0.12;
                panel.child(
                    h_flex()
                        .w_full()
                        .flex_none()
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
                                .child(format!(
                                    "⚠ 草稿自动保存失败：{err}（草稿可能无法跨重启恢复，请复制重要内容备份）"
                                )),
                        )
                        .child(
                            ramag_ui::clickable_button("draft-persist-retry")
                                .ghost()
                                .small()
                                .label("重试")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.schedule_draft_persist(cx);
                                })),
                        ),
                )
            })
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if !this.tabs.is_empty() {
                    let idx = this.active;
                    this.close_tab(idx, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .when(self.show_editor, |panel| {
                panel.child(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .border_b_1()
                        .border_color(border)
                        .bg(secondary_bg)
                        .child(
                            h_flex()
                                .id("query-tabs-scroll")
                                .flex_1()
                                .min_w_0()
                                .overflow_x_scroll()
                                .track_scroll(&self.tabs_scroll)
                                .children(tab_bar_items)
                                .child(
                                    ramag_ui::clickable_button("tab-add")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Plus)
                                        .when(!can_add_tab, |button| {
                                            button.tooltip(format!(
                                                "最多 {MAX_EDITOR_TABS} 个标签"
                                            ))
                                        })
                                        .disabled(!can_add_tab)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.add_tab(window, cx);
                                            },
                                        )),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .border_l_1()
                                .border_color(border)
                                .child(
                                    // 上游 IconName 无 History 变体，用旧版历史入口同款日历图标
                                    ramag_ui::clickable_button("query-history")
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
                                    let driver = self.connection.as_ref().map(|c| c.driver);
                                    // 模板个性化：用当前 Tab 最近点开的表名，没有则占位
                                    let table = self
                                        .tabs
                                        .get(self.active)
                                        .and_then(|t| {
                                            t.read(cx)
                                                .pinned_target
                                                .as_ref()
                                                .map(|(_, table)| table.clone())
                                        })
                                        .unwrap_or_default();
                                    ramag_ui::clickable_button("sql-examples")
                                        .ghost()
                                        .small()
                                        .icon(ramag_ui::icons::scroll_text())
                                        .tooltip("示例")
                                        .disabled(driver.is_none())
                                        .pointer_dropdown_menu(move |menu, _, _| {
                                            let Some(driver) = driver else {
                                                return menu;
                                            };
                                            let mut m = menu;
                                            for (label, sql) in
                                                super::super::query_tab::sql_examples(driver, &table)
                                            {
                                                let e = entity.clone();
                                                m = m.item(ramag_ui::menu_item(label).on_click(
                                                    move |_, window, app| {
                                                        e.update(app, |panel, cx| {
                                                            panel.insert_example_into_active(
                                                                &sql, window, cx,
                                                            );
                                                        });
                                                    },
                                                ));
                                            }
                                            m
                                        })
                                })
                                .child(
                                    ramag_ui::clickable_button("format-sql")
                                        .ghost()
                                        .small()
                                        .icon(ramag_ui::icons::wand_sparkles())
                                        .tooltip("格式化")
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                if let Some(tab) =
                                                    this.tabs.get(this.active).cloned()
                                                {
                                                    tab.update(cx, |t, cx| {
                                                        t.handle_format(window, cx)
                                                    });
                                                }
                                            },
                                        )),
                                )
                                .child(
                                    ramag_ui::clickable_button("explain-sql")
                                        .ghost()
                                        .small()
                                        .icon(ramag_ui::icons::gauge())
                                        .tooltip("执行计划")
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                if let Some(tab) =
                                                    this.tabs.get(this.active).cloned()
                                                {
                                                    tab.update(cx, |t, cx| {
                                                        t.handle_explain(window, cx)
                                                    });
                                                }
                                            },
                                        )),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(current_view, |this, view| this.child(view)),
            )
    }
}
