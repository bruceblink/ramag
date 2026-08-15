//! MySQL / PostgreSQL 表结构编辑器。

mod preview;
mod render;
mod sql;
#[cfg(test)]
mod tests;

use self::preview::{default_value_color, highlight_sql, render_ddl_panel, syntax_color};

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
