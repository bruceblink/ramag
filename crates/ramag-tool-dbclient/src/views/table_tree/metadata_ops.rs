//! 表树索引和触发器的展示、编辑与删除操作。

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement,
    SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt as _, PopupMenu},
    notification::Notification,
    v_flex,
};
use ramag_domain::entities::{Index, MAX_SQL_QUERY_BYTES, Trigger};

use super::{
    TableTreePanel,
    metadata_sql::{
        combine_ddl, index_create_sql, index_drop_sql, is_index_create_sql, is_trigger_create_sql,
        trigger_create_sql, trigger_drop_sql,
    },
};

#[derive(Clone)]
enum MetadataMenu {
    Index(Index),
    Trigger(Trigger),
}

struct MetadataRow {
    element_id: SharedString,
    text: String,
    copy_value: String,
    menu_kind: MetadataMenu,
    schema: String,
    table: String,
    color: gpui::Hsla,
}

pub(super) fn render_index_row(
    index: &Index,
    schema: String,
    table: String,
    index_index: usize,
    color: gpui::Hsla,
    cx: &mut Context<TableTreePanel>,
) -> AnyElement {
    render_metadata_row(
        MetadataRow {
            element_id: SharedString::from(format!(
                "tree-index-action-{schema}-{table}-{index_index}-{}",
                index.name
            )),
            text: format!("·  {}({})", index.name, index.columns.join(", ")),
            copy_value: index.name.clone(),
            menu_kind: MetadataMenu::Index(index.clone()),
            schema,
            table,
            color,
        },
        cx,
    )
}

pub(super) fn render_trigger_row(
    trigger: &Trigger,
    schema: String,
    table: String,
    trigger_index: usize,
    color: gpui::Hsla,
    cx: &mut Context<TableTreePanel>,
) -> AnyElement {
    render_metadata_row(
        MetadataRow {
            element_id: SharedString::from(format!(
                "tree-trigger-action-{schema}-{table}-{trigger_index}-{}",
                trigger.name
            )),
            text: format!("⚡ {} {} {}", trigger.timing, trigger.event, trigger.name),
            copy_value: trigger.name.clone(),
            menu_kind: MetadataMenu::Trigger(trigger.clone()),
            schema,
            table,
            color,
        },
        cx,
    )
}

/// 渲染索引或触发器行，并把复制和右键菜单绑定到同一条元数据记录。
fn render_metadata_row(row: MetadataRow, cx: &mut Context<TableTreePanel>) -> AnyElement {
    let MetadataRow {
        element_id,
        text,
        copy_value,
        menu_kind,
        schema,
        table,
        color,
    } = row;
    let entity = cx.entity().clone();
    let schema_for_menu = schema;
    let table_for_menu = table;
    div()
        .id(element_id)
        .w_full()
        .h(px(28.0))
        .flex_none()
        .pl(px(56.0))
        .pr_2()
        .pt(px(6.0))
        .text_xs()
        .text_color(color)
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .cursor_pointer()
        .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
            if ramag_ui::is_primary_modifier_double_click(event) {
                ramag_ui::copy_text_with_notification(copy_value.clone(), window, cx);
            }
        }))
        .context_menu(move |menu: PopupMenu, _, _| match &menu_kind {
            MetadataMenu::Index(index) => super::menus::index_context_menu(
                menu,
                entity.clone(),
                schema_for_menu.clone(),
                table_for_menu.clone(),
                index.clone(),
            ),
            MetadataMenu::Trigger(trigger) => super::menus::trigger_context_menu(
                menu,
                entity.clone(),
                schema_for_menu.clone(),
                table_for_menu.clone(),
                trigger.clone(),
            ),
        })
        .child(text)
        .into_any_element()
}

impl TableTreePanel {
    pub(super) fn open_index_update_dialog(
        &mut self,
        schema: String,
        table: String,
        index: Index,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index.primary {
            metadata_error("主键请通过表结构设计器修改", self, cx);
            return;
        }
        let Some(driver) = self.connection.as_ref().map(|config| config.driver) else {
            return;
        };
        let initial = match index_create_sql(driver, &schema, &table, &index) {
            Ok(sql) => sql,
            Err(error) => {
                metadata_error(error, self, cx);
                return;
            }
        };
        let entity = cx.entity().clone();
        let schema_for_submit = schema.clone();
        let table_for_submit = table.clone();
        let index_for_submit = index.clone();
        open_sql_editor(
            "更新索引",
            format!("编辑 {schema}.{table} 的索引 {}", index.name),
            initial,
            move |sql, _, app| {
                entity.update(app, |this, cx| {
                    this.update_index_sql(
                        schema_for_submit.clone(),
                        table_for_submit.clone(),
                        index_for_submit.clone(),
                        sql,
                        cx,
                    )
                })
            },
            window,
            cx,
        );
    }

    pub(super) fn drop_index(
        &mut self,
        schema: String,
        table: String,
        index: Index,
        cx: &mut Context<Self>,
    ) {
        if index.primary {
            metadata_error("主键不能从此处删除", self, cx);
            return;
        }
        let Some(driver) = self.connection.as_ref().map(|config| config.driver) else {
            return;
        };
        let sql = match index_drop_sql(driver, &schema, &table, &index) {
            Ok(sql) => sql,
            Err(error) => {
                metadata_error(error, self, cx);
                return;
            }
        };
        self.execute_metadata_ddl(sql, format!("已删除索引 {}", index.name), schema, table, cx);
    }

    fn update_index_sql(
        &mut self,
        schema: String,
        table: String,
        index: Index,
        edited_sql: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(driver) = self.connection.as_ref().map(|config| config.driver) else {
            return false;
        };
        if !is_index_create_sql(driver, &edited_sql) {
            metadata_error(
                "索引更新 SQL 必须是 CREATE INDEX 或 ALTER TABLE ... ADD INDEX",
                self,
                cx,
            );
            return false;
        }
        let drop_sql = match index_drop_sql(driver, &schema, &table, &index) {
            Ok(sql) => sql,
            Err(error) => {
                metadata_error(error, self, cx);
                return false;
            }
        };
        self.execute_metadata_ddl(
            combine_ddl(&drop_sql, &edited_sql),
            format!("已更新索引 {}", index.name),
            schema,
            table,
            cx,
        )
    }

    pub(super) fn open_trigger_update_dialog(
        &mut self,
        schema: String,
        table: String,
        trigger: Trigger,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(driver) = self.connection.as_ref().map(|config| config.driver) else {
            return;
        };
        let initial = match trigger_create_sql(driver, &schema, &table, &trigger) {
            Ok(sql) => sql,
            Err(error) => {
                metadata_error(error, self, cx);
                return;
            }
        };
        let entity = cx.entity().clone();
        let schema_for_submit = schema.clone();
        let table_for_submit = table.clone();
        let trigger_for_submit = trigger.clone();
        open_sql_editor(
            "更新触发器",
            format!("编辑 {schema}.{table} 的触发器 {}", trigger.name),
            initial,
            move |sql, _, app| {
                entity.update(app, |this, cx| {
                    this.update_trigger_sql(
                        schema_for_submit.clone(),
                        table_for_submit.clone(),
                        trigger_for_submit.clone(),
                        sql,
                        cx,
                    )
                })
            },
            window,
            cx,
        );
    }

    pub(super) fn drop_trigger(
        &mut self,
        schema: String,
        table: String,
        trigger: Trigger,
        cx: &mut Context<Self>,
    ) {
        let Some(driver) = self.connection.as_ref().map(|config| config.driver) else {
            return;
        };
        let sql = match trigger_drop_sql(driver, &schema, &table, &trigger.name) {
            Ok(sql) => sql,
            Err(error) => {
                metadata_error(error, self, cx);
                return;
            }
        };
        self.execute_metadata_ddl(
            sql,
            format!("已删除触发器 {}", trigger.name),
            schema,
            table,
            cx,
        );
    }

    fn update_trigger_sql(
        &mut self,
        schema: String,
        table: String,
        trigger: Trigger,
        edited_sql: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(driver) = self.connection.as_ref().map(|config| config.driver) else {
            return false;
        };
        if !is_trigger_create_sql(driver, &edited_sql) {
            metadata_error("触发器更新 SQL 必须是 CREATE TRIGGER", self, cx);
            return false;
        }
        let drop_sql = match trigger_drop_sql(driver, &schema, &table, &trigger.name) {
            Ok(sql) => sql,
            Err(error) => {
                metadata_error(error, self, cx);
                return false;
            }
        };
        self.execute_metadata_ddl(
            combine_ddl(&drop_sql, &edited_sql),
            format!("已更新触发器 {}", trigger.name),
            schema,
            table,
            cx,
        )
    }
}

type SqlEditorSubmit = dyn Fn(String, &mut Window, &mut App) -> bool;

fn open_sql_editor(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    initial: String,
    on_submit: impl Fn(String, &mut Window, &mut App) -> bool + 'static,
    window: &mut Window,
    cx: &mut Context<TableTreePanel>,
) {
    if initial.len() > MAX_SQL_QUERY_BYTES {
        ramag_ui::push_responsive_notification(
            window,
            Notification::error(format!(
                "DDL 内容超过 {} MiB，无法打开编辑对话框",
                MAX_SQL_QUERY_BYTES / 1024 / 1024
            )),
            cx,
        );
        return;
    }
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .code_editor("sql")
            .validate(|value, _| value.len() <= MAX_SQL_QUERY_BYTES)
            .rows(16)
            .default_value(initial)
    });
    ramag_ui::enforce_multiline_input_byte_limit(
        &input,
        MAX_SQL_QUERY_BYTES,
        window,
        cx,
        |this, _, cx| {
            this.pending_notification = Some(
                Notification::warning(format!(
                    "DDL 编辑内容最多 {} MiB，超出部分已截断",
                    MAX_SQL_QUERY_BYTES / 1024 / 1024
                ))
                .autohide(true),
            );
            cx.notify();
        },
    )
    .detach();
    input.update(cx, |state, cx| state.focus(window, cx));
    let on_submit: Rc<SqlEditorSubmit> = Rc::new(on_submit);
    let title = title.into();
    let description = description.into();
    window.open_dialog(cx, move |dialog, _, _| {
        let input_for_content = input.clone();
        let input_for_button = input.clone();
        let input_for_enter = input.clone();
        let submit_for_button = on_submit.clone();
        let submit_for_enter = on_submit.clone();
        let cancel = ramag_ui::clickable_button("metadata-sql-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| window.close_dialog(app));
        let apply = ramag_ui::clickable_button("metadata-sql-apply")
            .primary()
            .small()
            .label("确认")
            .on_click(move |_: &ClickEvent, window, app| {
                let sql = input_for_button.read(app).value().to_string();
                if submit_for_button(sql, window, app) {
                    window.close_dialog(app);
                }
            });
        dialog
            .title(ramag_ui::closable_dialog_title(
                "metadata-sql-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .width(px(820.0))
            .margin_top(px(90.0))
            .on_ok(move |_, window, app| {
                let sql = input_for_enter.read(app).value().to_string();
                submit_for_enter(sql, window, app)
            })
            .content({
                let description_for_content = description.clone();
                move |content, _, cx| {
                    content.child(
                        v_flex()
                            .gap(px(8.0))
                            .py(px(4.0))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(description_for_content.clone()),
                            )
                            .child(Input::new(&input_for_content).h(px(420.0))),
                    )
                }
            })
            .footer(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(cancel)
                    .child(apply),
            )
    });
}

fn metadata_error(
    message: impl Into<String>,
    this: &mut TableTreePanel,
    cx: &mut Context<TableTreePanel>,
) {
    this.pending_notification = Some(Notification::error(message.into()).autohide(true));
    cx.notify();
}
