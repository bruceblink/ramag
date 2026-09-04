use super::*;

impl Render for KeyTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            ramag_ui::push_responsive_notification(window, n, cx);
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let bg = theme.background;
        let row_hover = theme.muted;
        let accent = theme.accent;

        let total = self.keys.len();
        let in_search = !self.query.is_empty();
        let sink_same_name_keys = ramag_ui::redis_tree_settings(cx).sink_same_name_keys;
        let (visible_rc, visible_leaf_count) = self.visible_rows(sink_same_name_keys);
        let selected = self.selected.clone();
        let read_only = self.is_read_only();
        let mutating = self.mutation_gate.is_busy();

        // 状态栏：扫描中报进度；带服务端 MATCH 时标注模式，区别于「共 N（全库）」
        let pattern_note = self
            .match_pattern
            .as_deref()
            .map(|p| format!("MATCH {p} · "))
            .unwrap_or_default();
        let mut count_label = if self.config.is_none() {
            "尚未连接".to_string()
        } else if self.search_pending {
            format!("正在准备全库搜索“{}”…", self.query)
        } else if self.loading {
            format!("{pattern_note}已加载 {total} 个 key…（扫描中，可点⏹停止）")
        } else if let Some(ref e) = self.error {
            e.clone()
        } else if !in_search {
            format!(
                "{pattern_note}共 {total} 个 key{}",
                if self.truncated {
                    "（已停止，可继续扫描）"
                } else {
                    ""
                }
            )
        } else {
            format!("{pattern_note}匹配 {visible_leaf_count} / {total}")
        };
        if self.resource_limited {
            count_label.push_str(" · 已达到安全上限，请用 MATCH 缩小范围");
        } else if self.key_bytes >= INTERACTIVE_RESULT_WARNING_BYTES {
            count_label.push_str(" · 名称缓存已超过 128 MiB，建议用 MATCH 缩小范围");
        }
        if mutating {
            count_label.push_str(" · 写操作执行中…");
        }

        let current_db = self.db;
        let session_entity = cx.entity();
        let db_picker_label = format!("DB {current_db} ▾");
        let db_row = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .gap(px(8.0))
            .items_center()
            .child(
                ramag_ui::clickable_button("kt-db-picker")
                    .ghost()
                    .small()
                    .label(db_picker_label)
                    .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                        let mut m = menu;
                        let entity = session_entity.clone();
                        // 常规列 0-15；当前 db 更高（自建实例 databases > 16）时并入列表可回切
                        let mut dbs: Vec<u8> = (0u8..=15).collect();
                        if current_db > 15 {
                            dbs.push(current_db);
                        }
                        for db in dbs {
                            let is_active = db == current_db;
                            let label = if is_active {
                                format!("✓ DB {db}")
                            } else {
                                format!("  DB {db}")
                            };
                            let entity = entity.clone();
                            m = m.item(ramag_ui::menu_item(label).on_click(move |_, _, app| {
                                entity.update(app, |this, cx| {
                                    if this.db != db {
                                        cx.emit(KeyTreeEvent::DbSelected(db));
                                    }
                                });
                            }));
                        }
                        // 自建实例可配 databases > 16：提供自由输入入口（0-255）
                        let entity_for_prompt = session_entity.clone();
                        m = m.item(ramag_ui::menu_item("  自定义").on_click(
                            move |_, window, app| {
                                let entity = entity_for_prompt.clone();
                                ramag_ui::open_bounded_prompt(
                                    "切换 DB",
                                    "输入 DB 序号（0-255，须不超过服务端 databases 配置）"
                                        .to_string(),
                                    "",
                                    "切换",
                                    3,
                                    move |value, _window, app| match value.trim().parse::<u8>() {
                                        Ok(db) => {
                                            entity.update(app, |this, cx| {
                                                if this.db != db {
                                                    cx.emit(KeyTreeEvent::DbSelected(db));
                                                }
                                            });
                                        }
                                        Err(_) => {
                                            entity.update(app, |this, cx| {
                                                this.pending_notification = Some(
                                                    gpui_component::notification::Notification::error(
                                                        "DB 序号无效，请输入 0-255 的整数",
                                                    ),
                                                );
                                                cx.notify();
                                            });
                                        }
                                    },
                                    window,
                                    app,
                                );
                            },
                        ));
                        m
                    }),
            );

        let header = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .gap(px(6.0))
            .items_center()
            .child(
                div().flex_1().min_w_0().child(
                    ramag_ui::cleanable_input(&self.search, "redis-key-search-clear", false, cx)
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted_fg)),
                ),
            )
            .child({
                let scanning = self.loading;
                let icon = if scanning {
                    Icon::new(IconName::CircleX)
                } else {
                    ramag_ui::icons::refresh_cw()
                };
                ramag_ui::clickable_button("redis-key-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(if scanning { "停止扫描" } else { "刷新" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if scanning {
                            this.stop_scan(cx);
                        } else {
                            this.refresh(cx);
                        }
                    }))
            })
            .child({
                let any_expanded = visible_rc.iter().any(|row| row.is_expanded);
                let (icon, tip) = if any_expanded {
                    (IconName::FolderOpen, "折叠")
                } else {
                    (IconName::FolderClosed, "展开")
                };
                ramag_ui::clickable_button("redis-key-toggle-all")
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if any_expanded {
                            this.collapse_all(cx);
                        } else {
                            this.expand_all(cx);
                        }
                    }))
            })
            .child(
                ramag_ui::clickable_button("redis-open-console")
                    .ghost()
                    .xsmall()
                    .icon(IconName::SquareTerminal)
                    .tooltip("命令行")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        cx.emit(KeyTreeEvent::RequestOpenConsole);
                    })),
            )
            .child({
                let entity_for_menu = cx.entity().clone();
                let current_db = self.db;
                let more_tip: Option<&'static str> = if read_only {
                    Some("只读")
                } else if mutating {
                    Some("操作进行中")
                } else {
                    None
                };
                ramag_ui::clickable_button("redis-key-more")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::ellipsis())
                    .tooltip("更多")
                    .disabled(read_only || mutating)
                    .when_some(more_tip, |b, tip| b.tooltip(tip))
                    // 菜单顶部左角锚在按钮上，向右下方展开（不往上弹遮挡工具栏）
                    .pointer_dropdown_menu_with_anchor(gpui::Anchor::TopLeft, move |menu, _, _| {
                        ops::toolbar_more_menu(menu, entity_for_menu.clone(), current_db)
                    })
            });

        let theme_muted = theme.muted;

        let row_count = visible_rc.len();

        let empty_hint =
            !self.loading && total == 0 && self.config.is_some() && self.error.is_none();

        let body: gpui::AnyElement = if row_count == 0 {
            if self.search_pending {
                div()
                    .flex_1()
                    .min_h_0()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(muted_fg)
                    .child("正在全库搜索…")
                    .into_any_element()
            } else if empty_hint {
                // 空态分场景：服务端 MATCH 零命中 ≠ 空库；本地过滤零命中另有「匹配 0/N」计数
                let hint = match &self.match_pattern {
                    Some(p) => format!("没有匹配 MATCH {p} 的 key（服务端已全库扫描）"),
                    None => "DB 内没有 key".to_string(),
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(muted_fg)
                    .child(hint)
                    .into_any_element()
            } else if !self.loading && in_search && self.error.is_none() {
                div()
                    .flex_1()
                    .min_h_0()
                    .py(px(28.0))
                    .text_center()
                    .text_sm()
                    .text_color(muted_fg)
                    .child(format!("没有匹配“{}”的 key", self.query))
                    .into_any_element()
            } else {
                div().flex_1().min_h_0().into_any_element()
            }
        } else {
            let visible_for_closure = visible_rc.clone();
            let selected_for_closure = selected.clone();
            uniform_list(
                "redis-key-tree-rows",
                row_count,
                cx.processor(move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| {
                            let row_data = &visible_for_closure[i];
                            this.render_node_row(
                                i,
                                row_data,
                                &selected_for_closure,
                                fg,
                                muted_fg,
                                row_hover,
                                accent,
                                theme_muted,
                                cx,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.uniform_scroll)
            .flex_1()
            .into_any_element()
        };

        let can_load_more = self.truncated
            && !self.resource_limited
            && self.resume_cursor.is_some()
            && !self.loading;
        let status_bar = h_flex()
            .flex_none()
            .w_full()
            .items_center()
            .justify_between()
            .px(px(10.0))
            .py(px(4.0))
            .border_t_1()
            .border_color(border)
            .text_xs()
            .text_color(muted_fg)
            .child(count_label)
            .when(can_load_more, |bar| {
                bar.child(
                    ramag_ui::clickable_button("redis-key-load-more")
                        .ghost()
                        .xsmall()
                        .label("继续")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.load_more(cx);
                        })),
                )
            });

        let transfer_row = ramag_ui::transfer_progress_row(
            "redis-transfer-cancel",
            &self.transfer,
            |this: &mut Self| &this.transfer,
            cx,
        );

        v_flex()
            .size_full()
            .bg(bg)
            .child(db_row)
            .child(header)
            .children(transfer_row)
            .child(body)
            .child(status_bar)
    }
}
