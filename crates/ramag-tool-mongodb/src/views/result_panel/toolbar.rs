//! 结果区顶部工具栏：过滤列 / 过滤行 / 增删文档 / 导出 / 运行。
//! 行数 / 耗时摘要已下沉到底部 status bar（见 mod.rs render_status_bar），与 dbclient 一致

use gpui::{Anchor, Context, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
};
use ramag_ui::PointerDropdownMenu as _;
use ramag_ui::platform::primary_shortcut;

use super::{ResultEvent, ResultPanel};

pub(super) fn render(panel: &mut ResultPanel, cx: &mut Context<ResultPanel>) -> impl IntoElement {
    let secondary = cx.theme().secondary;
    let warning = cx.theme().warning;
    let production = panel.is_production();

    h_flex()
        .w_full()
        .flex_none()
        .px_3()
        .py(px(6.0))
        .gap_3()
        .items_center()
        .bg(secondary)
        .child(
            // 过滤列 + 过滤行：内层 flex_1 组（结构与间距均对齐 dbclient 过滤栏）
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child({
                    // 单行 InputState 仅在多行模式注册 up/down 的 on_action，单行下补全菜单无法用方向键导航；
                    // 这里把 MoveUp/MoveDown 转发给补全菜单（与 dbclient 过滤列同款 workaround）
                    let col_for_up = panel.column_filter.clone();
                    let col_for_down = panel.column_filter.clone();
                    div()
                        .flex_1()
                        .min_w_0()
                        .on_action(move |action: &gpui_component::input::MoveUp, window, app| {
                            col_for_up.update(app, |state, cx| {
                                state.handle_action_for_context_menu(
                                    Box::new(action.clone()),
                                    window,
                                    cx,
                                );
                            });
                        })
                        .on_action(
                            move |action: &gpui_component::input::MoveDown, window, app| {
                                col_for_down.update(app, |state, cx| {
                                    state.handle_action_for_context_menu(
                                        Box::new(action.clone()),
                                        window,
                                        cx,
                                    );
                                });
                            },
                        )
                        .child(
                            ramag_ui::cleanable_input(
                                &panel.column_filter,
                                "mongo-column-filter-clear",
                                false,
                                cx,
                            )
                            .small()
                            .bordered(false)
                            .focus_bordered(false),
                        )
                })
                .child(
                    div().flex_1().min_w_0().child(
                        ramag_ui::cleanable_input(
                            &panel.row_filter,
                            "mongo-row-filter-clear",
                            false,
                            cx,
                        )
                        .small()
                        .bordered(false)
                        .focus_bordered(false),
                    ),
                ),
        )
        // 生产只读徽标：常驻工具条，与连接 Tab 徽标、写入口禁用同一语义
        .when(production, |this| {
            let mut chip_bg = warning;
            chip_bg.a = 0.15;
            this.child(
                div()
                    .flex_none()
                    .px(px(6.0))
                    .py(px(1.0))
                    .rounded(px(4.0))
                    .bg(chip_bg)
                    .text_xs()
                    .text_color(warning)
                    .child("生产 · 只读"),
            )
        })
        .child({
            let can = panel.can_write();
            ramag_ui::clickable_button("mongo-insert")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .tooltip(if can {
                    "新增文档"
                } else if panel.doc_dml_busy {
                    "新增文档（上一写操作尚未完成）"
                } else if production {
                    "新增文档（生产连接 · 只读）"
                } else {
                    "新增文档（需当前命令指定 collection）"
                })
                .disabled(!can || panel.is_drilled())
                .on_click(cx.listener(|panel, _, window, cx| panel.open_insert_dialog(window, cx)))
        })
        .child({
            let can_del = panel.can_write()
                && !panel.selected_rows.is_empty()
                && !panel.is_drilled()
                && !panel.row_view_building
                && panel.row_view_error.is_none();
            ramag_ui::clickable_button("mongo-delete")
                .ghost()
                .small()
                .icon(IconName::Minus)
                .tooltip(if can_del {
                    "删除选中文档"
                } else if panel.row_view_building {
                    "删除选中文档（正在筛选 / 排序）"
                } else if panel.doc_dml_busy {
                    "删除选中文档（上一写操作尚未完成）"
                } else if production {
                    "删除选中文档（生产连接 · 只读）"
                } else {
                    "删除选中文档（先勾选行）"
                })
                .disabled(!can_del)
                .on_click(cx.listener(|panel, _, window, cx| panel.open_delete_confirm(window, cx)))
        })
        .child({
            let entity = cx.entity().clone();
            let has_data = panel.docs_arc.as_ref().is_some_and(|docs| !docs.is_empty());
            ramag_ui::clickable_button("mongo-export")
                .ghost()
                .small()
                .icon(ramag_ui::icons::download())
                .tooltip(if panel.exporting {
                    "导出进行中"
                } else if panel.row_view_building {
                    "导出（正在筛选 / 排序）"
                } else if panel.row_view_error.is_some() {
                    "导出（当前行视图构建失败）"
                } else {
                    "导出"
                })
                .disabled(
                    !has_data
                        || panel.table_building
                        || panel.row_view_building
                        || panel.row_view_error.is_some()
                        || panel.exporting,
                )
                .pointer_dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                    let e_json = entity.clone();
                    let e_csv = entity.clone();
                    menu.item(ramag_ui::menu_item("导出 JSON").on_click(move |_, _, app| {
                        e_json.update(app, |this, cx| this.export_documents(false, cx));
                    }))
                    .item(ramag_ui::menu_item("导出 CSV").on_click(move |_, _, app| {
                        e_csv.update(app, |this, cx| this.export_documents(true, cx));
                    }))
                })
        })
        .child(if panel.running {
            ramag_ui::clickable_button("mongo-cancel-result")
                .danger()
                .small()
                .icon(IconName::CircleX)
                .label("停止等待")
                .tooltip("停止客户端等待；服务器端操作可能仍在执行")
                .on_click(cx.listener(|_panel, _, _, cx| cx.emit(ResultEvent::Cancel)))
        } else {
            // 运行：与 dbclient 同位（结果区工具栏最右）、同快捷键（⌘↵）。
            ramag_ui::clickable_button("mongo-run-result")
                .primary()
                .small()
                .icon(IconName::Play)
                .tooltip(format!("{} 运行", primary_shortcut("Enter")))
                .on_click(cx.listener(|_panel, _, _, cx| cx.emit(ResultEvent::Refresh)))
        })
}
