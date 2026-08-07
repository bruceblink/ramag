//! 结果区顶部工具栏：过滤列 / 过滤行 / 增删文档 / 导出 / 运行。
//! 行数 / 耗时摘要已下沉到底部 status bar（见 mod.rs render_status_bar），与 dbclient 一致

use gpui::{ClickEvent, Context, Entity, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
};
use ramag_ui::PointerDropdownMenu as _;

use super::{ResultEvent, ResultPanel, RowSearchConversionStatus, RowSearchMode};

pub(super) fn render(panel: &mut ResultPanel, cx: &mut Context<ResultPanel>) -> impl IntoElement {
    let secondary = cx.theme().secondary;
    let warning = cx.theme().warning;
    let accent = cx.theme().accent;
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;
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
                .child({
                    let row_input = panel.row_filter.clone();
                    let row_search_mode = panel.row_search_mode();
                    let row_search_status = panel.row_search_conversion_status(cx);
                    let row_filter_has_value = !row_input.read(cx).value().is_empty();
                    let id_conversion_ready = ramag_ui::database_search_settings(cx).is_ready();
                    let panel_for_mode = cx.entity().clone();
                    div().flex_1().min_w_0().child(
                        Input::new(&row_input)
                            .small()
                            .bordered(false)
                            .focus_bordered(false)
                            .when(id_conversion_ready, |input| {
                                input.prefix(row_search_mode_button(
                                    row_search_mode,
                                    panel_for_mode,
                                    accent,
                                ))
                            })
                            .when(row_filter_has_value, |input| {
                                input.suffix(row_search_input_suffix(
                                    row_input,
                                    row_search_status,
                                    accent,
                                    muted,
                                    danger,
                                ))
                            }),
                    )
                }),
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
            let drilled = panel.is_drilled();
            let can_insert = can && !drilled;
            let disabled_reason = if panel.doc_dml_busy {
                "操作进行中"
            } else if production {
                "只读"
            } else if drilled {
                "请返回上层"
            } else {
                "请先打开集合"
            };
            ramag_ui::clickable_button("mongo-insert")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .when(!can_insert, |button| button.tooltip(disabled_reason))
                .disabled(!can_insert)
                .on_click(cx.listener(|panel, _, window, cx| panel.open_insert_dialog(window, cx)))
        })
        .child({
            let path_drilled = panel.parse_column_filter(cx).drill_path.is_some();
            let can_del = panel.can_write()
                && !panel.selected_rows.is_empty()
                && !panel.is_drilled()
                && !path_drilled
                && !panel.row_view_building
                && panel.row_view_error.is_none();
            let disabled_reason = if panel.row_view_building {
                "正在筛选"
            } else if panel.doc_dml_busy {
                "操作进行中"
            } else if production {
                "只读"
            } else if panel.is_drilled() {
                "请返回上层"
            } else if path_drilled {
                "请清空路径钻取"
            } else {
                "请先选择数据"
            };
            ramag_ui::clickable_button("mongo-delete")
                .ghost()
                .small()
                .icon(IconName::Minus)
                .when(!can_del, |button| button.tooltip(disabled_reason))
                .disabled(!can_del)
                .on_click(cx.listener(|panel, _, window, cx| panel.open_delete_confirm(window, cx)))
        })
        .child({
            let can_import = panel.can_write();
            let disabled_reason = if panel.doc_dml_busy {
                "操作进行中"
            } else if production {
                "只读"
            } else {
                "请先打开集合"
            };
            ramag_ui::clickable_button("mongo-import")
                .ghost()
                .small()
                .icon(ramag_ui::icons::download())
                .when(!can_import, |button| button.tooltip(disabled_reason))
                .disabled(!can_import)
                .on_click(
                    cx.listener(|panel, _, window, cx| panel.open_import_jsonl_dialog(window, cx)),
                )
        })
        .child({
            let has_data = panel.docs_arc.as_ref().is_some_and(|docs| !docs.is_empty());
            let path_drilled = panel.parse_column_filter(cx).drill_path.is_some();
            ramag_ui::clickable_button("mongo-export")
                .ghost()
                .small()
                .icon(ramag_ui::icons::upload())
                .when(path_drilled, |button| button.tooltip("请清空路径钻取"))
                .disabled(!has_data || panel.table_building || panel.exporting || path_drilled)
                .on_click(cx.listener(|panel, _, _, cx| panel.export_documents(cx)))
        })
        .child(if panel.running {
            ramag_ui::clickable_button("mongo-cancel-result")
                .danger()
                .small()
                .icon(IconName::CircleX)
                .label("停止")
                .tooltip("仅停止等待")
                .on_click(cx.listener(|_panel, _, _, cx| cx.emit(ResultEvent::Cancel)))
        } else {
            // 运行：与 dbclient 同位（结果区工具栏最右）、同快捷键（⌘↵）。
            ramag_ui::clickable_button("mongo-run-result")
                .primary()
                .small()
                .icon(IconName::Play)
                .on_click(cx.listener(|_panel, _, _, cx| cx.emit(ResultEvent::Refresh)))
        })
}

fn row_search_mode_button(
    current: RowSearchMode,
    panel: Entity<ResultPanel>,
    accent: gpui::Hsla,
) -> impl IntoElement {
    ramag_ui::clickable_button("mongo-row-search-mode")
        .text()
        .small()
        // 文本自带显式颜色，避免 Text 按钮按下态短暂继承主题前景色。
        .child(div().flex_none().text_color(accent).child(current.label()))
        .dropdown_caret(true)
        .text_color(accent)
        .tooltip(match current {
            RowSearchMode::Normal => "@TEXT：按单元格展示文本包含搜索",
            RowSearchMode::IdToInteger => "@ID -> I：将字符串转为整数，精确匹配整数单元格",
            RowSearchMode::IdToString => "@ID -> S：将非负十进制整数转为字符串，精确匹配文本单元格",
        })
        .pointer_dropdown_menu(move |mut menu, _, _| {
            for mode in RowSearchMode::ALL {
                let panel = panel.clone();
                menu = menu.item(
                    ramag_ui::menu_item(mode.label())
                        .checked(mode == current)
                        .on_click(move |_: &ClickEvent, _, app| {
                            panel.update(app, |panel, cx| {
                                panel.set_row_search_mode(mode, cx);
                            });
                        }),
                );
            }
            menu
        })
}

fn row_search_input_suffix(
    input: Entity<InputState>,
    status: Option<RowSearchConversionStatus>,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .when_some(status, |suffix, status| {
            suffix.child(row_search_conversion_label(status, accent, muted, danger))
        })
        .child(
            ramag_ui::clickable_button("mongo-row-filter-clear")
                .icon(Icon::new(IconName::CircleX))
                .ghost()
                .xsmall()
                .tab_stop(false)
                .text_color(muted)
                .on_click(move |_, window, cx| {
                    input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                }),
        )
}

fn row_search_conversion_label(
    status: RowSearchConversionStatus,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> gpui::AnyElement {
    let (label, color) = match status {
        RowSearchConversionStatus::Converting => ("→ 转换中…".to_string(), muted),
        RowSearchConversionStatus::Ready(output) => {
            (format!("→ {}", output.display_preview(40)), accent)
        }
        RowSearchConversionStatus::Error(_) => ("→ 转换失败".to_string(), danger),
    };

    div()
        .flex_none()
        .text_xs()
        .text_color(color)
        .child(label)
        .into_any_element()
}
