use super::*;

pub(super) fn collection_list_retained_bytes(
    database: &str,
    collections: &[MongoCollection],
    collection_capacity: usize,
) -> usize {
    collections.iter().fold(
        std::mem::size_of::<ExpandedState>()
            .saturating_add(std::mem::size_of::<String>())
            .saturating_add(database.len())
            .saturating_add(
                collection_capacity.saturating_mul(std::mem::size_of::<MongoCollection>()),
            ),
        |total, collection| {
            total
                .saturating_add(collection.name.capacity())
                .saturating_add(collection.database.capacity())
        },
    )
}

pub(super) fn prospective_collection_bytes(
    current: usize,
    previous: usize,
    replacement: usize,
) -> usize {
    current.saturating_sub(previous).saturating_add(replacement)
}

impl Render for CollectionTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            use gpui_component::WindowExt as _;
            window.push_notification(n, cx);
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let background = theme.background;

        let filter = self.current_filter(cx);

        let show_system = self.show_system;
        let active_label = self
            .active_db
            .clone()
            .unwrap_or_else(|| "未选库".to_string());
        let picker_label = format!("DB {active_label} ▾");
        let entity_for_picker = cx.entity().clone();
        let active_for_menu = self.active_db.clone();
        let picker_dbs: Vec<String> = self
            .databases
            .iter()
            .filter(|d| show_system || !is_system_db(&d.name))
            .map(|d| d.name.clone())
            .collect();
        let header = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .items_center()
            .gap(px(8.0))
            .child(
                ramag_ui::clickable_button("mongo-db-picker")
                    .ghost()
                    .small()
                    .label(picker_label)
                    .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        let mut m = menu;
                        let active = active_for_menu.clone();
                        for d in &picker_dbs {
                            let d_owned = d.clone();
                            let is_active = active.as_deref() == Some(d.as_str());
                            let label = if is_active {
                                format!("✓ {d}")
                            } else {
                                format!("  {d}")
                            };
                            let entity = entity_for_picker.clone();
                            m = m.item(ramag_ui::menu_item(label).on_click(move |_, _, app| {
                                let d = d_owned.clone();
                                entity.update(app, |this, cx| this.select_database(d, cx));
                            }));
                        }
                        m
                    }),
            );

        let editor_visible = self.editor_visible;
        let search_row = h_flex()
            .w_full()
            .items_center()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .gap(px(6.0))
            .child(
                div().flex_1().min_w_0().child(
                    ramag_ui::cleanable_input(&self.search, "mongo-tree-search-clear", false, cx)
                        .small()
                        .prefix(
                            gpui_component::Icon::new(gpui_component::IconName::Search)
                                .small()
                                .text_color(muted_fg),
                        ),
                ),
            )
            .child(
                ramag_ui::clickable_button("toggle-system-dbs")
                    .ghost()
                    .xsmall()
                    .icon(if show_system {
                        gpui_component::IconName::Eye
                    } else {
                        gpui_component::IconName::EyeOff
                    })
                    .tooltip("系统库")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_show_system(cx))),
            )
            .child(
                ramag_ui::clickable_button("refresh-mongo-tree")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                ramag_ui::clickable_button("toggle-mongo-editor")
                    .ghost()
                    .xsmall()
                    .icon(gpui_component::IconName::SquareTerminal)
                    .selected(editor_visible)
                    .tooltip("编辑器")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TreeEvent::ToggleEditor))),
            );

        let tree_view = self.tree_rows_view(&filter);
        let tree_rows = tree_view.rows;
        let body = uniform_list(
            "mongo-tree-rows",
            tree_rows.len(),
            cx.processor({
                let tree_rows = tree_rows.clone();
                move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| this.render_tree_row(&tree_rows[i], cx))
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.uniform_scroll)
        .px(px(2.0))
        .py(px(4.0))
        .flex_1();

        let total_dbs = self.databases.len();
        let visible_dbs = tree_view.visible_databases;
        let mut footer_text = if total_dbs == visible_dbs {
            format!("数据库 ({total_dbs})")
        } else {
            format!("数据库 ({visible_dbs}/{total_dbs})")
        };
        let searchable_dbs = self
            .databases
            .iter()
            .filter(|database| show_system || !is_system_db(&database.name))
            .count();
        if !filter.is_empty() && searchable_dbs > AUTO_LOAD_MAX_DATABASES {
            footer_text.push_str(" · 库过多，搜索仅覆盖已展开的库");
        }
        if self.mutation_gate.is_busy() {
            footer_text.push_str(" · 写操作执行中…");
        }

        let transfer_row = ramag_ui::transfer_progress_row(
            "mongo-transfer-cancel",
            &self.transfer,
            |this: &mut Self| &this.transfer,
            cx,
        );

        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(background)
            .child(header)
            .child(search_row)
            .children(transfer_row)
            .child(body)
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .px_2()
                    .py(px(4.0))
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted_fg)
                    .child(SharedString::from(footer_text)),
            )
    }
}
