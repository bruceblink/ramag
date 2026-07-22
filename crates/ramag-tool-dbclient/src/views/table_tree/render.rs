//! TableTreePanel Render：DB picker + 搜索 + 工具按钮 + uniform_list 行级虚拟化 + status bar

use std::ops::Range;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, Styled, Window, div, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Selectable as _, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, v_flex,
};
use ramag_domain::entities::DriverKind;
use ramag_ui::PointerDropdownMenu as _;

use super::{TableTreePanel, TreeEvent};
use crate::sql_completion::is_system_schema;

impl Render for TableTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 右键操作（清空/删除）异步完成的 toast 在这里推送
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }
        let muted_fg = cx.theme().muted_foreground;
        let red = gpui::red();

        // 早期返回
        if self.connection.is_none() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(muted_fg)
                .text_xs()
                .child("从左侧选一个连接")
                .into_any_element();
        }

        if self.loading_schemas {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(muted_fg)
                .text_xs()
                .child("加载 schemas…")
                .into_any_element();
        }

        if let Some(err) = self.error.clone() {
            return v_flex()
                .size_full()
                .p_2()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(red)
                        .child(format!("加载失败：{err}")),
                )
                .child(
                    ramag_ui::clickable_button("retry")
                        .small()
                        .label("重试")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.load_schemas(cx);
                        })),
                )
                .into_any_element();
        }

        // 派生树行按元数据代次、系统库开关和搜索词缓存。
        let show_system = self.show_system;
        let filter = self.current_filter(cx);
        let has_filter = !filter.is_empty();
        let tree_view = self.tree_rows_view(&filter);
        let total_schemas = self.schemas.len();
        let visible_schemas = tree_view.visible_schemas;
        let mut header_text = if total_schemas == visible_schemas {
            format!("数据库 ({total_schemas})")
        } else {
            format!("数据库 ({visible_schemas}/{total_schemas})")
        };
        let searchable_schemas = tree_view.searchable_schemas;
        let failed_schemas = tree_view.failed_schemas;
        let search_incomplete = has_filter && searchable_schemas < total_schemas;
        if search_incomplete {
            header_text.push_str(&format!(
                " · 当前搜索范围 {searchable_schemas}/{total_schemas} 个库"
            ));
        }
        if has_filter && failed_schemas > 0 {
            header_text.push_str(&format!(" · {failed_schemas} 个库加载失败"));
        }
        if self.ddl_gate.is_busy() {
            header_text.push_str(" · 结构变更执行中…");
        }
        let toggle_icon = if show_system {
            IconName::Eye
        } else {
            IconName::EyeOff
        };
        let qp_visible = self.editor_visible;
        // 顶部第 1 行：schema picker（与 Redis 的 DB picker 对齐布局）
        // PG：picker 显示 `database / schema`
        let driver = self.connection.as_ref().map(|c| c.driver);
        let pg_database: Option<String> = self
            .connection
            .as_ref()
            .filter(|c| matches!(c.driver, DriverKind::Postgres))
            .and_then(|c| c.database.clone());
        let active_label = self
            .active_schema
            .clone()
            .unwrap_or_else(|| "未选库".to_string());
        let picker_label = match (driver, pg_database.as_deref()) {
            (Some(DriverKind::Postgres), Some(db)) => {
                format!("DB {db} / {active_label} ▾")
            }
            _ => format!("DB {active_label} ▾"),
        };
        let entity_for_picker = cx.entity().clone();
        let picker_schemas: Vec<String> = self
            .schemas
            .iter()
            .filter(|s| show_system || !is_system_schema(&s.name))
            .map(|s| s.name.clone())
            .collect();
        let active_for_menu = self.active_schema.clone();

        let db_row = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .gap(px(8.0))
            .items_center()
            .child(
                ramag_ui::clickable_button("schema-picker")
                    .ghost()
                    .small()
                    .label(picker_label)
                    .pointer_dropdown_menu_with_anchor(
                        gpui::Anchor::BottomLeft,
                        move |menu, _, _| {
                            let mut m = menu;
                            let entity = entity_for_picker.clone();
                            let active = active_for_menu.clone();
                            for s in &picker_schemas {
                                let s_owned = s.clone();
                                let is_active = active.as_deref() == Some(s.as_str());
                                let label = if is_active {
                                    format!("✓ {s}")
                                } else {
                                    format!("  {s}")
                                };
                                let entity = entity.clone();
                                m = m.item(ramag_ui::menu_item(label).on_click(
                                    move |_, _, app| {
                                        let s = s_owned.clone();
                                        entity.update(app, |this, cx| {
                                            if this.active_schema.as_deref() != Some(s.as_str()) {
                                                this.active_schema = Some(s.clone());
                                                cx.emit(TreeEvent::SchemaActivated { schema: s });
                                                cx.notify();
                                            }
                                        });
                                    },
                                ));
                            }
                            m
                        },
                    ),
            );

        // 顶部第 2 行：搜索框 + 三个工具按钮
        let header_bar = h_flex()
            .w_full()
            .items_center()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .gap(px(6.0))
            .child(
                div().flex_1().min_w_0().child(
                    ramag_ui::cleanable_input(&self.search, "table-search-clear", false, cx)
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted_fg)),
                ),
            )
            .child(
                ramag_ui::clickable_button("toggle-system")
                    .ghost()
                    .xsmall()
                    .icon(toggle_icon)
                    .tooltip("系统库")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_show_system(cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("refresh-schemas")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.refresh(cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("toggle-query-panel")
                    .ghost()
                    .xsmall()
                    .icon(IconName::SquareTerminal)
                    .selected(qp_visible)
                    .tooltip("编辑器")
                    .on_click(cx.listener(|_this, _: &ClickEvent, _, cx| {
                        cx.emit(TreeEvent::ToggleSqlEditor);
                    })),
            );

        let can_retry_failed = failed_schemas > 0;
        let header_bar =
            if has_filter && search_incomplete && (total_schemas > 50 || can_retry_failed) {
                if let Some(progress) = self.full_search {
                    header_bar.child(
                        ramag_ui::clickable_button("stop-full-schema-search")
                            .small()
                            .label(format!("停止 {}/{}", progress.completed, progress.total))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cancel_full_search(cx);
                            })),
                    )
                } else {
                    let retry_only = failed_schemas > 0
                        && searchable_schemas.saturating_add(failed_schemas) == total_schemas;
                    header_bar.child(
                        ramag_ui::clickable_button("search-all-schemas")
                            .small()
                            .label(if retry_only {
                                "重试失败"
                            } else {
                                "搜索全部"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.load_all_tables_for_search(cx);
                            })),
                    )
                }
            } else {
                header_bar
            };

        // 导出 / 导入进行中：进度行 + 取消按钮
        let transfer_row = ramag_ui::transfer_progress_row(
            "table-transfer-cancel",
            &self.transfer,
            |this: &mut Self| &this.transfer,
            cx,
        );

        // uniform_list 行级虚拟化：仅渲染屏幕可见行
        let tree_rows_rc = tree_view.rows;
        let body = uniform_list(
            "mysql-tree-rows",
            tree_rows_rc.len(),
            cx.processor({
                let tree_rows_rc = tree_rows_rc.clone();
                move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| this.render_tree_row(&tree_rows_rc[i], cx))
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.uniform_scroll)
        .flex_1();

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(db_row)
            .child(header_bar)
            .children(transfer_row)
            .child(body)
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .px_2()
                    .py(px(4.0))
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(muted_fg)
                    .child(header_text),
            )
            .into_any_element()
    }
}
