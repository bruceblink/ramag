//! MySQL / PostgreSQL 图形化表结构编辑器。

use std::{collections::HashSet, rc::Rc};

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Render, ScrollHandle, Styled, StyledText, Subscription, Window, div, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    highlighter::{HighlightTheme, SyntaxColors, SyntaxHighlighter},
    input::{Input, InputEvent, InputState},
    notification::Notification,
    spinner::Spinner,
    v_flex,
};
use ramag_domain::entities::{Column, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES};
use ropey::Rope;

const NO_CHANGES: &str = "没有检测到表结构变更";
const FIELD_ROW_HEIGHT: f32 = 46.0;
const MAX_VISIBLE_FIELD_ROWS: usize = 8;
const SQL_PREVIEW_LINE_HEIGHT: f32 = 20.0;
const SQL_PREVIEW_VISIBLE_LINES: f32 = 6.0;
const SQL_PREVIEW_VERTICAL_PADDING: f32 = 24.0;
const TABLE_DDL_PANEL_HEIGHT: f32 = 420.0;

fn visible_field_rows(active_fields: usize, reviewing: bool) -> usize {
    let max_visible_rows = if reviewing {
        MAX_VISIBLE_FIELD_ROWS.saturating_sub(3)
    } else {
        MAX_VISIBLE_FIELD_ROWS
    };
    active_fields.clamp(1, max_visible_rows)
}

pub(super) type ExecuteHandler = Rc<dyn Fn(String, String, &mut Window, &mut App) -> bool>;
pub(super) type RenameHandler =
    Rc<dyn Fn(String, String, String, Entity<TableDesigner>, &mut Window, &mut App) -> bool>;

pub(super) struct TableDesignerConfig {
    pub(super) driver: DriverKind,
    pub(super) schema: String,
    pub(super) table: String,
    pub(super) columns: Vec<Column>,
    pub(super) loading: bool,
    pub(super) ddl_loading: bool,
    pub(super) on_execute: ExecuteHandler,
    pub(super) on_rename: RenameHandler,
}

struct FieldSql<'a> {
    name: &'a str,
    data_type: &'a str,
    default_value: &'a str,
    comment: &'a str,
}

pub(super) struct FieldDraft {
    name: Entity<InputState>,
    data_type: Entity<InputState>,
    nullable: bool,
    default_value: Entity<InputState>,
    comment: Entity<InputState>,
    original: Option<Column>,
    deleted: bool,
    _subscriptions: Vec<Subscription>,
}

pub(super) struct TableDesigner {
    driver: DriverKind,
    schema: String,
    original_table: String,
    table_name: Entity<InputState>,
    fields: Vec<FieldDraft>,
    field_scroll: ScrollHandle,
    sql_scroll: ScrollHandle,
    loading: bool,
    load_error: Option<String>,
    show_ddl: bool,
    ddl_loading: bool,
    ddl_text: Option<String>,
    ddl_error: Option<String>,
    preview_sql: Option<String>,
    discard_confirming: bool,
    executing: bool,
    on_execute: ExecuteHandler,
    on_rename: RenameHandler,
    _table_name_subscription: Subscription,
}

impl TableDesigner {
    pub(super) fn new(
        config: TableDesignerConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let table_name = Self::input(&config.table, window, cx);
        let table_name_subscription = Self::subscribe_input(&table_name, cx);
        let fields = config
            .columns
            .into_iter()
            .map(|column| Self::from_column(column, window, cx))
            .collect();
        Self {
            driver: config.driver,
            schema: config.schema,
            original_table: config.table,
            table_name,
            fields,
            field_scroll: ScrollHandle::new(),
            sql_scroll: ScrollHandle::new(),
            loading: config.loading,
            load_error: None,
            show_ddl: false,
            ddl_loading: config.ddl_loading,
            ddl_text: None,
            ddl_error: None,
            preview_sql: None,
            discard_confirming: false,
            executing: false,
            on_execute: config.on_execute,
            on_rename: config.on_rename,
            _table_name_subscription: table_name_subscription,
        }
    }

    fn input(value: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        cx.new(|cx| InputState::new(window, cx).default_value(value))
    }

    fn subscribe_input(input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.preview_sql = None;
                this.discard_confirming = false;
                cx.notify();
            }
        })
    }

    fn from_column(column: Column, window: &mut Window, cx: &mut Context<Self>) -> FieldDraft {
        let name = Self::input(&column.name, window, cx);
        let data_type = Self::input(&column.data_type.raw_type, window, cx);
        let default_value = Self::input(column.default_value.as_deref().unwrap_or(""), window, cx);
        let comment = Self::input(column.comment.as_deref().unwrap_or(""), window, cx);
        let subscriptions = [&name, &data_type, &default_value, &comment]
            .into_iter()
            .map(|input| Self::subscribe_input(input, cx))
            .collect();
        FieldDraft {
            name,
            data_type,
            nullable: column.nullable,
            default_value,
            comment,
            original: Some(column),
            deleted: false,
            _subscriptions: subscriptions,
        }
    }

    fn new_field(
        name_value: &str,
        data_type_value: &str,
        nullable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FieldDraft {
        let name = Self::input(name_value, window, cx);
        let data_type = Self::input(data_type_value, window, cx);
        let default_value = Self::input("", window, cx);
        let comment = Self::input("", window, cx);
        let subscriptions = [&name, &data_type, &default_value, &comment]
            .into_iter()
            .map(|input| Self::subscribe_input(input, cx))
            .collect();
        FieldDraft {
            name,
            data_type,
            nullable,
            default_value,
            comment,
            original: None,
            deleted: false,
            _subscriptions: subscriptions,
        }
    }

    fn add_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.next_field_name(cx);
        self.fields
            .push(Self::new_field(&name, "VARCHAR(255)", true, window, cx));
        self.preview_sql = None;
        self.discard_confirming = false;
        self.field_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn next_field_name(&self, cx: &gpui::App) -> String {
        let used: HashSet<_> = self
            .fields
            .iter()
            .filter(|field| !field.deleted)
            .map(|field| field.name.read(cx).value().trim().to_ascii_lowercase())
            .collect();
        for index in 1.. {
            let candidate = if index == 1 {
                "new_column".to_string()
            } else {
                format!("new_column_{index}")
            };
            if !used.contains(&candidate) {
                return candidate;
            }
        }
        unreachable!("字段名候选序列不会耗尽")
    }

    pub(super) fn set_columns(
        &mut self,
        columns: Vec<Column>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields = columns
            .into_iter()
            .map(|column| Self::from_column(column, window, cx))
            .collect();
        self.loading = false;
        self.load_error = None;
        self.preview_sql = None;
        self.discard_confirming = false;
        cx.notify();
    }

    pub(super) fn set_ddl(&mut self, ddl: String, cx: &mut Context<Self>) {
        self.ddl_loading = false;
        self.ddl_text = Some(ddl);
        self.ddl_error = None;
        cx.notify();
    }

    pub(super) fn set_ddl_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.ddl_loading = false;
        self.ddl_error = Some(error);
        cx.notify();
    }

    pub(super) fn set_load_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.loading = false;
        self.load_error = Some(error);
        cx.notify();
    }

    fn build_preview(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        self.preview_sql = Some(self.change_sql(cx)?);
        self.discard_confirming = false;
        cx.notify();
        Ok(())
    }

    fn save_table_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.executing {
            return;
        }
        let sql = match self.rename_sql(cx) {
            Ok(sql) => sql,
            Err(error) => {
                window.push_notification(Notification::warning(error).autohide(true), cx);
                return;
            }
        };
        let old_table = self.original_table.clone();
        let new_table = self.table_name.read(cx).value().trim().to_string();
        self.executing = true;
        cx.notify();
        if !(self.on_rename)(sql, old_table, new_table, cx.entity(), window, cx) {
            self.executing = false;
            cx.notify();
        }
    }

    pub(super) fn finish_rename(
        &mut self,
        success: bool,
        new_table: String,
        cx: &mut Context<Self>,
    ) {
        self.executing = false;
        if success {
            self.original_table = new_table;
            self.preview_sql = None;
            self.ddl_loading = true;
            self.ddl_text = None;
            self.ddl_error = None;
        }
        cx.notify();
    }

    /// 返回 true 时调用方可直接关闭；有未执行变更时改为显示弹窗内确认区。
    pub(super) fn allow_dialog_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.executing {
            return false;
        }
        if !self.has_changes(cx) {
            return true;
        }
        self.discard_confirming = true;
        cx.notify();
        false
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.allow_dialog_close(cx) {
            window.close_dialog(cx);
        }
    }

    fn confirm_execute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.executing {
            return;
        }
        // 确认时重新生成，避免预览后继续编辑导致执行旧 SQL。
        let sql = match self.change_sql(cx) {
            Ok(sql) => sql,
            Err(error) => {
                window.push_notification(Notification::warning(error).autohide(true), cx);
                return;
            }
        };
        self.executing = true;
        cx.notify();
        if (self.on_execute)(sql, self.original_table.clone(), window, cx) {
            window.close_dialog(cx);
        } else {
            self.executing = false;
            cx.notify();
        }
    }

    pub(super) fn change_sql(&self, cx: &gpui::App) -> Result<String, String> {
        let qualified = format!(
            "{}.{}",
            self.driver.quote_identifier(&self.schema),
            self.driver.quote_identifier(&self.original_table)
        );
        let mut statements = Vec::new();
        let mut mysql_alter_clauses = Vec::new();
        let mut names = HashSet::new();
        for field in &self.fields {
            let name = field.name.read(cx).value().trim().to_string();
            let data_type = field.data_type.read(cx).value().trim().to_string();
            let default_value = field.default_value.read(cx).value().trim().to_string();
            let comment = field.comment.read(cx).value().trim().to_string();
            if field.deleted {
                if let Some(original) = &field.original {
                    let column = self.driver.quote_identifier(&original.name);
                    if self.driver == DriverKind::Mysql {
                        mysql_alter_clauses.push(format!("DROP COLUMN {column}"));
                    } else {
                        statements.push(format!("ALTER TABLE {qualified} DROP COLUMN {column};"));
                    }
                }
                continue;
            }
            validate_identifier("字段名", &name)?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(format!("字段名 {name} 重复，请修改后再预览"));
            }
            if data_type.is_empty() {
                return Err(format!("字段 {name} 的类型不能为空"));
            }
            if data_type.contains(';') {
                return Err(format!("字段 {name} 的类型不能包含分号"));
            }
            if default_value.contains(';') {
                return Err(format!("字段 {name} 的默认值不能包含分号"));
            }
            let sql = FieldSql {
                name: &name,
                data_type: &data_type,
                default_value: &default_value,
                comment: &comment,
            };
            match self.driver {
                DriverKind::Mysql => self.mysql_field_sql(field, &sql, &mut mysql_alter_clauses),
                DriverKind::Postgres => {
                    self.postgres_field_sql(field, &qualified, &sql, &mut statements)
                }
                _ => return Err("当前数据库不支持表结构设计器".into()),
            }
        }
        if !mysql_alter_clauses.is_empty() {
            statements.push(format!(
                "ALTER TABLE {qualified} {};",
                mysql_alter_clauses.join(",\n    ")
            ));
        }
        if statements.is_empty() {
            Err(NO_CHANGES.into())
        } else {
            Ok(statements.join("\n"))
        }
    }

    fn rename_sql(&self, cx: &gpui::App) -> Result<String, String> {
        let table = self.table_name.read(cx).value().trim().to_string();
        validate_identifier("表名", &table)?;
        if self.original_table == table {
            return Err(NO_CHANGES.into());
        }
        let schema = self.driver.quote_identifier(&self.schema);
        let old = self.driver.quote_identifier(&self.original_table);
        let new = self.driver.quote_identifier(&table);
        match self.driver {
            DriverKind::Mysql => Ok(format!("RENAME TABLE {schema}.{old} TO {schema}.{new};")),
            DriverKind::Postgres => Ok(format!("ALTER TABLE {schema}.{old} RENAME TO {new};")),
            _ => Err("当前数据库不支持表结构设计器".into()),
        }
    }

    fn has_changes(&self, cx: &gpui::App) -> bool {
        self.has_table_name_change(cx) || self.has_field_changes(cx)
    }

    fn has_table_name_change(&self, cx: &gpui::App) -> bool {
        self.original_table != self.table_name.read(cx).value().trim()
    }

    fn has_field_changes(&self, cx: &gpui::App) -> bool {
        self.fields.iter().any(|field| match &field.original {
            None => !field.deleted,
            Some(_) if field.deleted => true,
            Some(original) => field_changed(
                field,
                original,
                field.name.read(cx).value().trim(),
                field.data_type.read(cx).value().trim(),
                field.default_value.read(cx).value().trim(),
                field.comment.read(cx).value().trim(),
            ),
        })
    }

    fn mysql_field_sql(&self, field: &FieldDraft, sql: &FieldSql<'_>, out: &mut Vec<String>) {
        let definition = mysql_definition(
            self.driver,
            sql.name,
            sql.data_type,
            field.nullable,
            sql.default_value,
            sql.comment,
        );
        match &field.original {
            None => out.push(format!("ADD COLUMN {definition}")),
            Some(original)
                if field_changed(
                    field,
                    original,
                    sql.name,
                    sql.data_type,
                    sql.default_value,
                    sql.comment,
                ) =>
            {
                out.push(format!(
                    "CHANGE COLUMN {} {definition}",
                    self.driver.quote_identifier(&original.name)
                ))
            }
            _ => {}
        }
    }

    fn postgres_field_sql(
        &self,
        field: &FieldDraft,
        table: &str,
        sql: &FieldSql<'_>,
        out: &mut Vec<String>,
    ) {
        let qname = self.driver.quote_identifier(sql.name);
        let Some(original) = &field.original else {
            let null = if field.nullable { "" } else { " NOT NULL" };
            let default = if sql.default_value.is_empty() {
                String::new()
            } else {
                format!(" DEFAULT {}", sql.default_value)
            };
            out.push(format!(
                "ALTER TABLE {table} ADD COLUMN {qname} {}{null}{default};",
                sql.data_type
            ));
            if !sql.comment.is_empty() {
                out.push(format!(
                    "COMMENT ON COLUMN {table}.{qname} IS '{}';",
                    escape_literal(sql.comment)
                ));
            }
            return;
        };
        if original.name != sql.name {
            out.push(format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {qname};",
                self.driver.quote_identifier(&original.name)
            ));
        }
        if original.data_type.raw_type != sql.data_type {
            out.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {qname} TYPE {};",
                sql.data_type
            ));
        }
        if original.nullable != field.nullable {
            out.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {qname} {};",
                if field.nullable {
                    "DROP NOT NULL"
                } else {
                    "SET NOT NULL"
                }
            ));
        }
        if original.default_value.as_deref().unwrap_or("") != sql.default_value {
            out.push(if sql.default_value.is_empty() {
                format!("ALTER TABLE {table} ALTER COLUMN {qname} DROP DEFAULT;")
            } else {
                format!(
                    "ALTER TABLE {table} ALTER COLUMN {qname} SET DEFAULT {};",
                    sql.default_value
                )
            });
        }
        if original.comment.as_deref().unwrap_or("") != sql.comment {
            out.push(format!(
                "COMMENT ON COLUMN {table}.{qname} IS {};",
                if sql.comment.is_empty() {
                    "NULL".into()
                } else {
                    format!("'{}'", escape_literal(sql.comment))
                }
            ));
        }
    }
}

impl Render for TableDesigner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted;
        let muted_fg = theme.muted_foreground;
        let syntax = &theme.highlight_theme.style.syntax;
        let type_color = syntax_color(syntax, "type", theme.link);
        let keyword_color = syntax_color(syntax, "keyword", theme.info);
        let number_color = syntax_color(syntax, "number", theme.warning);
        let string_color = syntax_color(syntax, "string", theme.success);
        let constant_color = syntax_color(syntax, "constant", theme.foreground);
        let entity = cx.entity().clone();
        if self.loading {
            return v_flex()
                .w_full()
                .h(px(360.0))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new().small())
                .child(div().text_sm().child("正在加载字段结构…"))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted_fg)
                        .child("加载完成后可直接编辑，无需再次点击。"),
                );
        }
        if let Some(error) = &self.load_error {
            return v_flex()
                .w_full()
                .h(px(300.0))
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("字段结构加载失败"),
                )
                .child(
                    div()
                        .max_w(px(680.0))
                        .text_xs()
                        .text_color(muted_fg)
                        .child(error.clone()),
                );
        }
        let active_fields = self.fields.iter().filter(|field| !field.deleted).count();
        let mut rows = v_flex().w_full();
        let reviewing = self.preview_sql.is_some();
        let visible_rows = visible_field_rows(active_fields, reviewing);
        let rows_height = px(visible_rows as f32 * FIELD_ROW_HEIGHT);
        for (index, field) in self.fields.iter().enumerate() {
            if field.deleted {
                continue;
            }
            let toggle = entity.clone();
            let remove = entity.clone();
            rows =
                rows.child(
                    h_flex()
                        .w_full()
                        .min_h(px(46.0))
                        .items_center()
                        .gap_2()
                        .px_3()
                        .border_t_1()
                        .border_color(border)
                        .when(index % 2 == 1, |row| row.bg(muted.opacity(0.45)))
                        .child(
                            Input::new(&field.name)
                                .w(px(170.0))
                                .disabled(reviewing || self.executing),
                        )
                        .child(
                            Input::new(&field.data_type)
                                .w(px(180.0))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(type_color)
                                .disabled(reviewing || self.executing),
                        )
                        .child(
                            h_flex().w(px(76.0)).items_center().justify_center().child(
                                ramag_ui::clickable_checkbox(format!("field-nullable-{index}"))
                                    .checked(field.nullable)
                                    .small()
                                    .disabled(reviewing || self.executing)
                                    .tooltip("勾选表示该字段允许 NULL")
                                    .on_click(move |nullable: &bool, _, app| {
                                        toggle.update(app, |this, cx| {
                                            if let Some(field) = this.fields.get_mut(index) {
                                                field.nullable = *nullable;
                                            }
                                            this.preview_sql = None;
                                            this.discard_confirming = false;
                                            cx.notify();
                                        })
                                    }),
                            ),
                        )
                        .child(
                            Input::new(&field.default_value)
                                .w(px(180.0))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(default_value_color(
                                    field.default_value.read(cx).value().as_ref(),
                                    keyword_color,
                                    number_color,
                                    string_color,
                                    constant_color,
                                ))
                                .disabled(reviewing || self.executing),
                        )
                        .child(div().flex_1().min_w(px(150.0)).child(
                            Input::new(&field.comment).disabled(reviewing || self.executing),
                        ))
                        .child(
                            ramag_ui::clickable_button(format!("field-delete-{index}"))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Delete)
                                .tooltip("删除字段")
                                .text_color(theme.danger)
                                .disabled(reviewing || self.executing)
                                .on_click(move |_: &ClickEvent, _, app| {
                                    remove.update(app, |this, cx| {
                                        if let Some(field) = this.fields.get_mut(index) {
                                            field.deleted = true;
                                        }
                                        this.preview_sql = None;
                                        this.discard_confirming = false;
                                        cx.notify();
                                    })
                                }),
                        ),
                );
        }
        let add = entity.clone();
        let preview = entity.clone();
        let execute = entity.clone();
        let rename = entity.clone();
        let toggle_ddl = entity.clone();
        let preview_cancel = entity.clone();
        let edit_cancel = entity.clone();
        let continue_editing = entity.clone();
        let executing = self.executing;
        let has_field_changes = self.has_field_changes(cx);
        let has_table_name_change = self.has_table_name_change(cx);
        let show_ddl = self.show_ddl;
        let discard_confirming = self.discard_confirming;
        let preview_sql = if discard_confirming {
            None
        } else {
            self.preview_sql.clone()
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted_fg).child("表名"))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Input::new(&self.table_name)
                                            .w(px(320.0))
                                            .disabled(reviewing || executing || show_ddl),
                                    )
                                    .when(has_table_name_change, |name| {
                                        name.child(
                                            ramag_ui::clickable_button("table-designer-save-name")
                                                .primary()
                                                .small()
                                                .label(if executing {
                                                    "保存中…"
                                                } else {
                                                    "保存"
                                                })
                                                .loading(executing)
                                                .disabled(reviewing || executing || show_ddl)
                                                .on_click(move |_: &ClickEvent, window, app| {
                                                    rename.update(app, |this, cx| {
                                                        this.save_table_name(window, cx)
                                                    });
                                                }),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        ramag_ui::clickable_button("table-designer-show-ddl")
                            .secondary()
                            .small()
                            .label(if show_ddl {
                                "返回字段设计"
                            } else {
                                "查看建表语句"
                            })
                            .disabled(reviewing || executing)
                            .on_click(move |_: &ClickEvent, _, app| {
                                toggle_ddl.update(app, |this, cx| {
                                    this.show_ddl = !this.show_ddl;
                                    this.discard_confirming = false;
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .when(show_ddl, |designer| {
                designer.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("建表语句"),
                        )
                        .child(render_ddl_panel(
                            self.ddl_loading,
                            self.ddl_text.clone(),
                            self.ddl_error.clone(),
                            &self.sql_scroll,
                            theme,
                        )),
                )
            })
            .when(!show_ddl, |designer| {
                designer.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("字段结构"),
                                )
                                .child(div().text_xs().text_color(muted_fg).child(format!(
                                    "共 {active_fields} 个字段。修改完成后先预览 SQL，再确认执行。"
                                ))),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .border_1()
                                .border_color(border)
                                .rounded_lg()
                                .overflow_hidden()
                                .child(
                                    h_flex()
                                        .min_h(px(38.0))
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .bg(muted.opacity(0.7))
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(muted_fg)
                                        .child(div().w(px(170.0)).child("字段名"))
                                        .child(div().w(px(180.0)).child("类型"))
                                        .child(div().w(px(76.0)).text_center().child("允许 NULL"))
                                        .child(div().w(px(180.0)).child("默认值"))
                                        .child(div().flex_1().child("注释"))
                                        .child(div().w(px(36.0)).text_center().child("操作")),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .h(rows_height)
                                        .min_h_0()
                                        .flex_none()
                                        .id("table-designer-fields-scroll")
                                        .overflow_y_scroll()
                                        .track_scroll(&self.field_scroll)
                                        .child(rows),
                                ),
                        ),
                )
            })
            .when_some(preview_sql, |designer, sql| {
                let mono = theme.mono_font_family.clone();
                let highlighted_sql = highlight_sql(sql, &theme.highlight_theme);
                designer.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .p_3()
                        .border_1()
                        .border_color(border)
                        .rounded_lg()
                        .bg(muted.opacity(0.55))
                        .when(!discard_confirming, |preview| {
                            preview
                                .child(
                                    h_flex()
                                        .w_full()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("SQL 预览"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted_fg)
                                                .child("请确认后再执行"),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(SQL_PREVIEW_LINE_HEIGHT * SQL_PREVIEW_VISIBLE_LINES
                                            + SQL_PREVIEW_VERTICAL_PADDING))
                                        .id("table-designer-sql-preview-scroll")
                                        .overflow_y_scroll()
                                        .track_scroll(&self.sql_scroll)
                                        .p_3()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(border)
                                        .bg(theme.background)
                                        .text_xs()
                                        .line_height(px(SQL_PREVIEW_LINE_HEIGHT))
                                        .font_family(mono)
                                        .whitespace_normal()
                                        .child(highlighted_sql),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            ramag_ui::clickable_button("modify-table-back-to-edit")
                                                .ghost()
                                                .small()
                                                .label("返回修改")
                                                .disabled(executing)
                                                .on_click({
                                                    let entity = entity.clone();
                                                    move |_: &ClickEvent, _, app| {
                                                        entity.update(app, |this, cx| {
                                                            this.preview_sql = None;
                                                            cx.notify();
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    ramag_ui::clickable_button(
                                                        "modify-table-preview-cancel",
                                                    )
                                                    .ghost()
                                                    .small()
                                                    .label("取消")
                                                    .disabled(executing)
                                                    .on_click(move |_: &ClickEvent, window, app| {
                                                        preview_cancel.update(app, |this, cx| {
                                                            this.request_close(window, cx)
                                                        });
                                                    }),
                                                )
                                                .child(
                                                    ramag_ui::clickable_button(
                                                        "modify-table-confirm-execute",
                                                    )
                                                    .primary()
                                                    .small()
                                                    .label(if executing {
                                                        "执行中…"
                                                    } else {
                                                        "确认执行"
                                                    })
                                                    .loading(executing)
                                                    .disabled(executing)
                                                    .on_click(move |_: &ClickEvent, window, app| {
                                                        execute.update(app, |this, cx| {
                                                            this.confirm_execute(window, cx)
                                                        });
                                                    }),
                                                ),
                                        ),
                                )
                        }),
                )
            })
            .when(
                self.preview_sql.is_none() && !discard_confirming,
                |designer| {
                    designer.child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .when(!show_ddl, |actions| {
                                actions.child(
                                    ramag_ui::clickable_button("field-add")
                                        .secondary()
                                        .small()
                                        .label("＋ 添加字段")
                                        .disabled(executing)
                                        .on_click(move |_, window, app| {
                                            add.update(app, |this, cx| this.add_field(window, cx))
                                        }),
                                )
                            })
                            .when(show_ddl, |actions| actions.child(div()))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        ramag_ui::clickable_button("modify-table-cancel")
                                            .ghost()
                                            .small()
                                            .label("取消")
                                            .on_click(move |_: &ClickEvent, window, app| {
                                                edit_cancel.update(app, |this, cx| {
                                                    this.request_close(window, cx)
                                                });
                                            }),
                                    )
                                    .when(has_field_changes && !show_ddl, |actions| {
                                        actions.child(
                                            ramag_ui::clickable_button("modify-table-preview")
                                                .primary()
                                                .small()
                                                .label("预览变更")
                                                .disabled(executing || has_table_name_change)
                                                .when(has_table_name_change, |button| {
                                                    button.tooltip("请先保存表名")
                                                })
                                                .on_click(move |_: &ClickEvent, window, app| {
                                                    preview.update(app, |this, cx| {
                                                        match this.build_preview(cx) {
                                                            Ok(()) => {}
                                                            Err(error) => window.push_notification(
                                                                Notification::warning(error)
                                                                    .autohide(true),
                                                                cx,
                                                            ),
                                                        }
                                                    });
                                                }),
                                        )
                                    }),
                            ),
                    )
                },
            )
            .when(discard_confirming, |designer| {
                designer.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .p_3()
                        .border_1()
                        .border_color(theme.danger.opacity(0.35))
                        .rounded_lg()
                        .bg(theme.danger.opacity(0.08))
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("放弃未执行的变更？"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_fg)
                                        .child("字段新增、修改、删除及表名变更都将丢失。"),
                                ),
                        )
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    ramag_ui::clickable_button("modify-table-continue-editing")
                                        .secondary()
                                        .small()
                                        .label("继续编辑")
                                        .on_click(move |_: &ClickEvent, _, app| {
                                            continue_editing.update(app, |this, cx| {
                                                this.discard_confirming = false;
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    ramag_ui::clickable_button("modify-table-discard-changes")
                                        .danger()
                                        .small()
                                        .label("放弃更改")
                                        .on_click(|_: &ClickEvent, window, app| {
                                            window.close_dialog(app)
                                        }),
                                ),
                        ),
                )
            })
    }
}

fn field_changed(
    field: &FieldDraft,
    original: &Column,
    name: &str,
    data_type: &str,
    default_value: &str,
    comment: &str,
) -> bool {
    original.name != name
        || original.data_type.raw_type != data_type
        || original.nullable != field.nullable
        || original.default_value.as_deref().unwrap_or("") != default_value
        || original.comment.as_deref().unwrap_or("") != comment
}

fn validate_identifier(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    if value.len() > MAX_CONNECTION_IDENTIFIER_BYTES {
        return Err(format!(
            "{label}不能超过 {MAX_CONNECTION_IDENTIFIER_BYTES} 字节"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
}

fn render_ddl_panel(
    loading: bool,
    ddl: Option<String>,
    error: Option<String>,
    scroll: &ScrollHandle,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let content = if loading {
        v_flex()
            .w_full()
            .h(px(TABLE_DDL_PANEL_HEIGHT))
            .items_center()
            .justify_center()
            .gap_2()
            .child(Spinner::new().small())
            .child(div().text_sm().child("正在加载建表语句…"))
            .into_any_element()
    } else if let Some(error) = error {
        v_flex()
            .w_full()
            .h(px(TABLE_DDL_PANEL_HEIGHT))
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_sm().child("建表语句加载失败"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(error),
            )
            .into_any_element()
    } else {
        let ddl = ddl.unwrap_or_else(|| "暂无建表语句".into());
        let highlighted_ddl = highlight_sql(ddl, &theme.highlight_theme);
        div()
            .w_full()
            .h(px(TABLE_DDL_PANEL_HEIGHT))
            .id("table-designer-ddl-scroll")
            .overflow_y_scroll()
            .track_scroll(scroll)
            .p_3()
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .whitespace_normal()
            .child(highlighted_ddl)
            .into_any_element()
    };
    div()
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded_lg()
        .bg(theme.background)
        .overflow_hidden()
        .child(content)
}

fn highlight_sql(sql: String, theme: &HighlightTheme) -> StyledText {
    let mut highlighter = SyntaxHighlighter::new("sql");
    highlighter.update(None, &Rope::from_str(&sql), None);
    let highlights = highlighter.styles(&(0..sql.len()), theme);
    StyledText::new(sql).with_highlights(highlights)
}

fn mysql_definition(
    driver: DriverKind,
    name: &str,
    data_type: &str,
    nullable: bool,
    default_value: &str,
    comment: &str,
) -> String {
    let null = if nullable { " NULL" } else { " NOT NULL" };
    let default = if default_value.is_empty() {
        String::new()
    } else {
        format!(" DEFAULT {default_value}")
    };
    let comment = if comment.is_empty() {
        String::new()
    } else {
        format!(" COMMENT '{}'", escape_literal(comment))
    };
    format!(
        "{} {data_type}{null}{default}{comment}",
        driver.quote_identifier(name)
    )
}

fn escape_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn syntax_color(syntax: &SyntaxColors, name: &str, fallback: Hsla) -> Hsla {
    syntax
        .style(name)
        .and_then(|style| style.color)
        .unwrap_or(fallback)
}

fn default_value_color(
    value: &str,
    keyword: Hsla,
    number: Hsla,
    string: Hsla,
    constant: Hsla,
) -> Hsla {
    let value = value.trim();
    if value.is_empty() {
        return constant;
    }
    if value.parse::<f64>().is_ok() {
        return number;
    }
    if (value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('"') && value.ends_with('"'))
    {
        return string;
    }
    if matches!(
        value.to_ascii_uppercase().as_str(),
        "NULL" | "TRUE" | "FALSE" | "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP"
    ) || value.to_ascii_uppercase().starts_with("CURRENT_TIMESTAMP(")
    {
        return keyword;
    }
    constant
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use ramag_domain::entities::{ColumnKind, ColumnType};

    fn column(name: &str, raw_type: &str, nullable: bool) -> Column {
        Column {
            name: name.into(),
            data_type: ColumnType {
                kind: ColumnKind::Other,
                raw_type: raw_type.into(),
            },
            nullable,
            default_value: None,
            is_primary_key: false,
            comment: None,
        }
    }

    fn designer(
        driver: DriverKind,
        columns: Vec<Column>,
        cx: &mut TestAppContext,
    ) -> (Entity<TableDesigner>, &mut gpui::VisualTestContext) {
        let mut designer = None;
        let (_, visual_cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                TableDesigner::new(
                    TableDesignerConfig {
                        driver,
                        schema: "public".into(),
                        table: "users".into(),
                        columns,
                        loading: false,
                        ddl_loading: false,
                        on_execute: Rc::new(|_, _, _, _| true),
                        on_rename: Rc::new(|_, _, _, _, _, _| true),
                    },
                    window,
                    cx,
                )
            });
            designer = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let Some(designer) = designer else {
            unreachable!("测试窗口应创建表设计器")
        };
        (designer, visual_cx)
    }

    #[gpui::test]
    fn unchanged_columns_do_not_generate_sql(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(DriverKind::Mysql, vec![column("id", "int", false)], cx);

        let result = cx.update(|_, app| designer.read(app).change_sql(app));

        assert_eq!(result, Err(NO_CHANGES.into()));
        assert!(!cx.update(|_, app| designer.read(app).has_changes(app)));
    }

    #[gpui::test]
    fn closing_changed_designer_requires_confirmation(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);

        let unchanged_can_close = cx
            .update(|_, app| designer.update(app, |designer, cx| designer.allow_dialog_close(cx)));
        assert!(unchanged_can_close);

        cx.update(|window, app| {
            designer.update(app, |designer, cx| designer.add_field(window, cx));
        });
        let changed_can_close = cx
            .update(|_, app| designer.update(app, |designer, cx| designer.allow_dialog_close(cx)));

        assert!(!changed_can_close);
        assert!(cx.update(|_, app| designer.read(app).discard_confirming));
    }

    #[gpui::test]
    fn added_fields_receive_unique_names(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(
            DriverKind::Mysql,
            vec![column("new_column", "varchar(255)", true)],
            cx,
        );
        cx.update(|window, app| {
            designer.update(app, |designer, cx| {
                designer.add_field(window, cx);
                designer.add_field(window, cx);
            });
        });

        let names = cx.update(|_, app| {
            designer
                .read(app)
                .fields
                .iter()
                .map(|field| field.name.read(app).value().to_string())
                .collect::<Vec<_>>()
        });

        assert_eq!(names, ["new_column", "new_column_2", "new_column_3"]);
    }

    #[gpui::test]
    fn duplicate_field_names_are_rejected(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(
            DriverKind::Mysql,
            vec![
                column("first_name", "varchar(255)", true),
                column("last_name", "varchar(255)", true),
            ],
            cx,
        );
        cx.update(|window, app| {
            designer.update(app, |designer, cx| {
                designer.fields[1]
                    .name
                    .update(cx, |input, cx| input.set_value("first_name", window, cx));
            });
        });

        let error = cx
            .update(|_, app| designer.read(app).change_sql(app))
            .expect_err("重复字段名必须阻止 SQL 生成");

        assert!(error.contains("字段名 first_name 重复"));
    }

    #[gpui::test]
    fn table_rename_uses_database_dialect(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for (driver, expected) in [
            (
                DriverKind::Mysql,
                "RENAME TABLE `public`.`users` TO `public`.`members`;",
            ),
            (
                DriverKind::Postgres,
                "ALTER TABLE \"public\".\"users\" RENAME TO \"members\";",
            ),
        ] {
            let (designer, visual_cx) = designer(driver, Vec::new(), cx);
            visual_cx.update(|window, app| {
                designer.update(app, |designer, cx| {
                    designer
                        .table_name
                        .update(cx, |input, cx| input.set_value("members", window, cx));
                });
            });

            let sql = visual_cx
                .update(|_, app| designer.read(app).rename_sql(app))
                .expect("改表名应生成 SQL");
            assert_eq!(sql, expected);
        }
    }

    #[gpui::test]
    fn table_name_change_is_not_included_in_field_sql(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);
        cx.update(|window, app| {
            designer.update(app, |designer, cx| {
                designer
                    .table_name
                    .update(cx, |input, cx| input.set_value("members", window, cx));
            });
        });

        let (field_sql, table_changed, fields_changed) = cx.update(|_, app| {
            let designer = designer.read(app);
            (
                designer.change_sql(app),
                designer.has_table_name_change(app),
                designer.has_field_changes(app),
            )
        });

        assert_eq!(field_sql, Err(NO_CHANGES.into()));
        assert!(table_changed);
        assert!(!fields_changed);
    }

    #[gpui::test]
    fn mysql_add_column_uses_mysql_dialect(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);
        cx.update(|window, app| {
            designer.update(app, |designer, cx| designer.add_field(window, cx));
        });

        let sql = cx
            .update(|_, app| designer.read(app).change_sql(app))
            .expect("新增字段应生成 SQL");

        assert_eq!(
            sql,
            "ALTER TABLE `public`.`users` ADD COLUMN `new_column` VARCHAR(255) NULL;"
        );
    }

    #[gpui::test]
    fn mysql_batches_multiple_column_changes_into_one_alter(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);
        cx.update(|window, app| {
            designer.update(app, |designer, cx| {
                designer.add_field(window, cx);
                designer.add_field(window, cx);
            });
        });

        let sql = cx
            .update(|_, app| designer.read(app).change_sql(app))
            .expect("多个字段应生成 SQL");

        assert_eq!(sql.matches("ALTER TABLE").count(), 1);
        assert_eq!(sql.matches("ADD COLUMN").count(), 2);
        assert!(sql.contains(",\n    ADD COLUMN"));
    }

    #[gpui::test]
    fn postgres_changes_emit_separate_alter_statements(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (designer, cx) = designer(DriverKind::Postgres, vec![column("name", "text", true)], cx);
        cx.update(|window, app| {
            designer.update(app, |designer, cx| {
                let field = &mut designer.fields[0];
                field.nullable = false;
                field
                    .comment
                    .update(cx, |input, cx| input.set_value("显示名称", window, cx));
            });
        });

        let sql = cx
            .update(|_, app| designer.read(app).change_sql(app))
            .expect("修改字段应生成 SQL");

        assert_eq!(
            sql,
            "ALTER TABLE \"public\".\"users\" ALTER COLUMN \"name\" SET NOT NULL;\n\
             COMMENT ON COLUMN \"public\".\"users\".\"name\" IS '显示名称';"
        );
    }

    #[test]
    fn default_value_semantics_choose_expected_colors() {
        let keyword = gpui::red();
        let number = gpui::green();
        let string = gpui::blue();
        let constant = gpui::white();

        assert_eq!(
            default_value_color("CURRENT_TIMESTAMP", keyword, number, string, constant),
            keyword
        );
        assert_eq!(
            default_value_color("42.5", keyword, number, string, constant),
            number
        );
        assert_eq!(
            default_value_color("'draft'", keyword, number, string, constant),
            string
        );
    }

    #[test]
    fn many_fields_use_fixed_visible_row_count() {
        assert_eq!(visible_field_rows(14, false), MAX_VISIBLE_FIELD_ROWS);
        assert_eq!(visible_field_rows(14, true), 5);
        assert_eq!(visible_field_rows(0, false), 1);
    }
}
