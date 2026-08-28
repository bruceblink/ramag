//! 表树右键菜单。

use gpui::Entity;
use gpui_component::menu::PopupMenu;
use ramag_domain::entities::{DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES};
use ramag_ui::{open_bounded_prompt, open_confirm};

use super::TableTreePanel;

pub(super) fn table_context_menu(
    menu: PopupMenu,
    entity: Entity<TableTreePanel>,
    schema: String,
    table: String,
    is_view: bool,
) -> PopupMenu {
    let table_for_copy = table.clone();
    let menu = menu.item(
        ramag_ui::menu_item("复制表名").on_click(move |_, window, app| {
            ramag_ui::copy_text_with_notification(table_for_copy.clone(), window, app);
        }),
    );

    let (schema_for_properties, table_for_properties, entity_for_properties) =
        (schema.clone(), table.clone(), entity.clone());
    let menu = menu.item(ramag_ui::menu_item("属性").on_click(move |_, _, app| {
        entity_for_properties.update(app, |this, cx| {
            this.handle_show_table_properties(
                schema_for_properties.clone(),
                table_for_properties.clone(),
                is_view,
                cx,
            )
        });
    }));

    let menu = if is_view {
        let (schema, table, entity) = (schema.clone(), table.clone(), entity.clone());
        menu.item(ramag_ui::menu_item("视图定义").on_click(move |_, _, app| {
            let (schema, table) = (schema.clone(), table.clone());
            entity.update(app, |this, cx| {
                this.handle_show_ddl(schema, table, true, cx)
            });
        }))
    } else {
        let (export_schema, export_table, export_entity) =
            (schema.clone(), table.clone(), entity.clone());
        let menu = menu.item(ramag_ui::menu_item("导出").on_click(move |_, _, app| {
            let (schema, table) = (export_schema.clone(), export_table.clone());
            export_entity.update(app, |this, cx| this.export_table_to_file(schema, table, cx));
        }));
        let (modify_schema, modify_table, modify_entity) =
            (schema.clone(), table.clone(), entity.clone());
        let menu = menu.item(ramag_ui::menu_item("修改表").on_click(move |_, _, app| {
            modify_entity.update(app, |this, cx| {
                this.handle_modify_table(modify_schema.clone(), modify_table.clone(), cx)
            });
        }));
        let (compare_schema, compare_table, compare_entity) =
            (schema.clone(), table.clone(), entity.clone());
        menu.item(
            ramag_ui::menu_item("比较表结构").on_click(move |_, window, app| {
                compare_entity.update(app, |this, cx| {
                    this.prompt_compare_table(
                        compare_schema.clone(),
                        compare_table.clone(),
                        window,
                        cx,
                    )
                });
            }),
        )
    }
    .separator();

    let menu = if is_view {
        let (schema, table, entity) = (schema.clone(), table.clone(), entity.clone());
        menu.item(ramag_ui::menu_item("改名").on_click(move |_, window, app| {
            let (schema, table, entity) = (schema.clone(), table.clone(), entity.clone());
            open_bounded_prompt(
                "重命名视图",
                format!("输入 {schema}.{table} 的新名称"),
                &table.clone(),
                "改名",
                MAX_CONNECTION_IDENTIFIER_BYTES,
                move |new_name, _, app| {
                    entity.update(app, |this, cx| {
                        this.rename_table(schema, table, new_name, true, cx)
                    });
                },
                window,
                app,
            );
        }))
    } else {
        menu
    };

    let menu = if is_view {
        menu
    } else {
        let (schema, table, entity) = (schema.clone(), table.clone(), entity.clone());
        menu.item(
            ramag_ui::menu_item("清空表").on_click(move |_, window, app| {
                let (schema, table, entity) = (schema.clone(), table.clone(), entity.clone());
                open_double_confirm(
                    "清空表",
                    format!("将删除 {schema}.{table} 的全部数据，此操作不可恢复。"),
                    "再次确认清空表",
                    format!("请再次确认：永久删除 {schema}.{table} 的全部数据。"),
                    "确认清空",
                    move |_, app| {
                        entity.update(app, |this, cx| this.truncate_table(schema, table, cx));
                    },
                    window,
                    app,
                );
            }),
        )
    };

    let (label, description) = if is_view {
        (
            "删除视图",
            format!("将删除视图 {schema}.{table}，底层表数据不受影响。"),
        )
    } else {
        (
            "删除表",
            format!("将永久删除表 {schema}.{table} 及其数据，此操作不可恢复。"),
        )
    };
    menu.item(ramag_ui::menu_item(label).on_click(move |_, window, app| {
        let (schema, table, entity) = (schema.clone(), table.clone(), entity.clone());
        let second_title = if is_view {
            "再次确认删除视图"
        } else {
            "再次确认删除表"
        };
        let second_description = if is_view {
            format!("请再次确认：永久删除视图 {schema}.{table}。")
        } else {
            format!("请再次确认：永久删除表 {schema}.{table} 及其数据。")
        };
        let second_confirm_label = if is_view {
            "确认删除视图"
        } else {
            "确认删除表"
        };
        open_double_confirm(
            label,
            description.clone(),
            second_title,
            second_description,
            second_confirm_label,
            move |_, app| {
                entity.update(app, |this, cx| this.drop_table(schema, table, is_view, cx));
            },
            window,
            app,
        );
    }))
}

/// 先关闭第一次确认，再打开第二次确认，避免弹窗关闭动作误关掉第二个弹窗。
#[allow(clippy::too_many_arguments)]
fn open_double_confirm(
    first_title: impl Into<gpui::SharedString>,
    first_description: impl Into<gpui::SharedString>,
    second_title: impl Into<gpui::SharedString>,
    second_description: impl Into<gpui::SharedString>,
    second_confirm_label: impl Into<gpui::SharedString>,
    on_confirm: impl FnOnce(&mut gpui::Window, &mut gpui::App) + 'static,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let second_title = second_title.into();
    let second_description = second_description.into();
    let second_confirm_label = second_confirm_label.into();
    let window_handle = window.window_handle();

    open_confirm(
        first_title,
        first_description,
        "继续",
        true,
        move |_, app| {
            let second_title = second_title.clone();
            let second_description = second_description.clone();
            let second_confirm_label = second_confirm_label.clone();
            app.defer(move |app| {
                let _ = window_handle.update(app, move |_, window, app| {
                    open_confirm(
                        second_title,
                        second_description,
                        second_confirm_label,
                        true,
                        on_confirm,
                        window,
                        app,
                    );
                });
            });
        },
        window,
        cx,
    );
}

pub(super) fn schema_context_menu(
    menu: PopupMenu,
    entity: Entity<TableTreePanel>,
    schema: String,
    driver: DriverKind,
) -> PopupMenu {
    let (schema_for_export, entity_for_export) = (schema.clone(), entity.clone());
    let menu = menu.item(ramag_ui::menu_item("导出").on_click(move |_, _, app| {
        let (schema, entity) = (schema_for_export.clone(), entity_for_export.clone());
        entity.update(app, |this, cx| this.export_schema_to_file(schema, cx));
    }));

    let (schema_for_import, entity_for_import) = (schema.clone(), entity.clone());
    let menu = menu.item(
        ramag_ui::menu_item("导入库").on_click(move |_, window, app| {
            let (schema, entity) = (schema_for_import.clone(), entity_for_import.clone());
            ramag_ui::open_import_options_dialog(
                "导入库",
                format!("选择 SQL 文件导入 {schema}。Ramag 导出保留原库；其他 SQL 导入当前库。"),
                true,
                ("SQL", &["sql"]),
                move |policy, files, _, app| {
                    entity.update(app, |this, cx| {
                        this.import_schema_from_files(schema, policy, files, cx);
                    });
                },
                window,
                app,
            );
        }),
    );

    let (schema_for_tables, entity_for_tables) = (schema.clone(), entity.clone());
    let menu = menu
        .item(
            ramag_ui::menu_item("导入表").on_click(move |_, window, app| {
                let (schema, entity) = (schema_for_tables.clone(), entity_for_tables.clone());
                ramag_ui::open_import_options_dialog(
                    "导入表",
                    format!("选择 Ramag 表 SQL，恢复结构和数据到 {schema}（文件库须一致）。"),
                    true,
                    ("SQL", &["sql"]),
                    move |policy, files, _, app| {
                        entity.update(app, |this, cx| {
                            this.import_structured_tables_from_files(schema, policy, files, cx);
                        });
                    },
                    window,
                    app,
                );
            }),
        )
        .separator();

    let (schema_for_diagram, entity_for_diagram) = (schema.clone(), entity.clone());
    let menu = menu.item(
        ramag_ui::menu_item("Schema Diagram").on_click(move |_, _, app| {
            entity_for_diagram.update(app, |this, cx| {
                this.handle_show_schema_diagram(schema_for_diagram.clone(), cx);
            });
        }),
    );

    let menu = if driver == DriverKind::Postgres {
        let (schema, entity) = (schema.clone(), entity.clone());
        menu.item(ramag_ui::menu_item("改名").on_click(move |_, window, app| {
            let (schema, entity) = (schema.clone(), entity.clone());
            open_bounded_prompt(
                "重命名 Schema",
                format!("输入 {schema} 的新名称"),
                &schema.clone(),
                "改名",
                MAX_CONNECTION_IDENTIFIER_BYTES,
                move |new_name, _, app| {
                    entity.update(app, |this, cx| this.rename_schema(schema, new_name, cx));
                },
                window,
                app,
            );
        }))
    } else {
        menu
    };

    let (title, description) = if driver == DriverKind::Postgres {
        (
            "删除 Schema",
            format!("将永久删除 {schema} 及其中全部对象，此操作不可恢复。"),
        )
    } else {
        (
            "删除数据库",
            format!("将永久删除 {schema} 及其中全部表和数据，此操作不可恢复。"),
        )
    };
    menu.item(ramag_ui::menu_item("删除").on_click(move |_, window, app| {
        let (schema, entity) = (schema.clone(), entity.clone());
        open_confirm(
            title,
            description.clone(),
            "删除",
            true,
            move |_, app| {
                entity.update(app, |this, cx| this.drop_schema(schema, cx));
            },
            window,
            app,
        );
    }))
}
