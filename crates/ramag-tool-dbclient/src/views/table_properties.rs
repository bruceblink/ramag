//! 单表结构只读视图：按 DataGrip 风格分组展示列、键、索引、外键和触发器。

use std::{collections::BTreeSet, sync::Arc};

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Render, ScrollHandle, Styled,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Theme,
    button::ButtonVariants as _,
    h_flex,
    scroll::{Scrollbar, ScrollbarShow},
    spinner::Spinner,
    v_flex,
};
use ramag_app::ConnectionService;
use ramag_domain::entities::{Column, ConnectionConfig, ForeignKey, Index, Query, Trigger, Value};
use tracing::error;

mod ddl;
mod render;

use self::ddl::render_ddl;
use self::render::{
    OutlineCounts, is_key, render_columns, render_foreign_keys, render_indexes, render_outline,
    render_section, render_triggers, render_warnings,
};

const VIEW_HEIGHT: f32 = 650.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum TablePropertiesSection {
    Columns,
    Keys,
    Indexes,
    ForeignKeys,
    Triggers,
}

impl TablePropertiesSection {
    const ALL: [Self; 5] = [
        Self::Columns,
        Self::Keys,
        Self::Indexes,
        Self::ForeignKeys,
        Self::Triggers,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TablePropertiesTab {
    #[default]
    Structure,
    Ddl,
}

#[derive(Clone, Debug, Default)]
struct LoadedTableStructure {
    columns: Vec<Column>,
    indexes: Vec<Index>,
    foreign_keys: Vec<ForeignKey>,
    triggers: Vec<Trigger>,
    warnings: Vec<String>,
}

pub(crate) struct TablePropertiesDialog {
    service: Arc<ConnectionService>,
    connection: ConnectionConfig,
    schema: String,
    table: String,
    is_view: bool,
    structure: Option<LoadedTableStructure>,
    loading: bool,
    ddl_loading: bool,
    ddl_text: Option<String>,
    ddl_error: Option<String>,
    active_tab: TablePropertiesTab,
    expanded_sections: BTreeSet<TablePropertiesSection>,
    request_generation: u64,
    vertical_scroll: ScrollHandle,
    ddl_vertical_scroll: ScrollHandle,
    ddl_horizontal_scroll: ScrollHandle,
}

impl TablePropertiesDialog {
    pub(crate) fn new(
        service: Arc<ConnectionService>,
        connection: ConnectionConfig,
        schema: String,
        table: String,
        is_view: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            service,
            connection,
            schema,
            table,
            is_view,
            structure: None,
            loading: false,
            ddl_loading: false,
            ddl_text: None,
            ddl_error: None,
            active_tab: TablePropertiesTab::default(),
            expanded_sections: TablePropertiesSection::ALL.into_iter().collect(),
            request_generation: 0,
            vertical_scroll: ScrollHandle::new(),
            ddl_vertical_scroll: ScrollHandle::new(),
            ddl_horizontal_scroll: ScrollHandle::new(),
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let request_generation = self.request_generation;
        let service = self.service.clone();
        let connection = self.connection.clone();
        let schema = self.schema.clone();
        let table = self.table.clone();
        let is_view = self.is_view;
        self.loading = true;
        self.ddl_loading = true;
        self.ddl_text = None;
        self.ddl_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let ddl_service = service.clone();
            let ddl_connection = connection.clone();
            let (structure, ddl) = futures::join!(
                load_table_structure(service, connection.clone(), schema.clone(), table.clone()),
                load_table_ddl(ddl_service, ddl_connection, schema, table, is_view),
            );
            let _ = this.update(cx, |this, cx| {
                if this.request_generation != request_generation
                    || this.connection.id != connection.id
                {
                    return;
                }
                this.structure = Some(structure);
                this.loading = false;
                this.ddl_loading = false;
                match ddl {
                    Ok(ddl) => this.ddl_text = Some(ddl),
                    Err(error) => {
                        error!(
                            operation = "table_properties_ddl_load",
                            connection_id = %this.connection.id,
                            schema = %this.schema,
                            table = %this.table,
                            error = %error,
                            "load table properties DDL failed"
                        );
                        this.ddl_error = Some(format!("加载建表语句失败：{error:#}"));
                    }
                }
                this.vertical_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                this.ddl_vertical_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                this.ddl_horizontal_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                cx.notify();
            });
        })
        .detach();
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(7.0))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .bg(theme.secondary)
            .child(
                Icon::new(if self.is_view {
                    IconName::Frame
                } else {
                    IconName::MemoryStick
                })
                .small()
                .text_color(theme.accent),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("{}.{}", self.schema, self.table)),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if self.is_view {
                            "视图结构"
                        } else {
                            "表结构"
                        },
                    )),
            )
            .child(
                div()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(3.0))
                    .bg(theme.muted)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("只读"),
            )
            .child(
                ramag_ui::clickable_button("table-properties-refresh")
                    .ghost()
                    .small()
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("重新加载元数据")
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    fn render_tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_none()
            .gap(px(4.0))
            .child(
                ramag_ui::clickable_button("table-properties-structure-tab")
                    .small()
                    .label("结构")
                    .when(self.active_tab == TablePropertiesTab::Structure, |button| {
                        button.primary()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.active_tab = TablePropertiesTab::Structure;
                        cx.notify();
                    })),
            )
            .child(
                ramag_ui::clickable_button("table-properties-ddl-tab")
                    .small()
                    .label("DDL")
                    .when(self.active_tab == TablePropertiesTab::Ddl, |button| {
                        button.primary()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.active_tab = TablePropertiesTab::Ddl;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if self.is_view {
                        "只读视图定义"
                    } else {
                        "只读建表语句"
                    }),
            )
    }

    fn render_structure(
        &self,
        structure: &LoadedTableStructure,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let keys = structure
            .indexes
            .iter()
            .filter(|index| is_key(index))
            .collect::<Vec<_>>();
        let indexes = structure
            .indexes
            .iter()
            .filter(|index| !is_key(index))
            .collect::<Vec<_>>();

        let mut sections = v_flex().w_full().gap(px(10.0));
        if !structure.warnings.is_empty() {
            sections = sections.child(render_warnings(&structure.warnings, theme));
        }
        if self
            .expanded_sections
            .contains(&TablePropertiesSection::Columns)
        {
            sections = sections.child(render_section(
                "列",
                structure.columns.len(),
                IconName::MemoryStick,
                render_columns(&structure.columns, theme),
                theme,
            ));
        }
        if self
            .expanded_sections
            .contains(&TablePropertiesSection::Keys)
        {
            sections = sections.child(render_section(
                "键",
                keys.len(),
                IconName::File,
                render_indexes(&keys, theme, true),
                theme,
            ));
        }
        if self
            .expanded_sections
            .contains(&TablePropertiesSection::Indexes)
        {
            sections = sections.child(render_section(
                "索引",
                indexes.len(),
                IconName::File,
                render_indexes(&indexes, theme, false),
                theme,
            ));
        }
        if self
            .expanded_sections
            .contains(&TablePropertiesSection::ForeignKeys)
        {
            sections = sections.child(render_section(
                "外键",
                structure.foreign_keys.len(),
                IconName::ArrowRight,
                render_foreign_keys(&structure.foreign_keys, theme),
                theme,
            ));
        }
        if self
            .expanded_sections
            .contains(&TablePropertiesSection::Triggers)
        {
            sections = sections.child(render_section(
                "触发器",
                structure.triggers.len(),
                IconName::Network,
                render_triggers(&structure.triggers, theme),
                theme,
            ));
        }

        let outline = render_outline(
            OutlineCounts {
                columns: structure.columns.len(),
                keys: keys.len(),
                indexes: indexes.len(),
                foreign_keys: structure.foreign_keys.len(),
                triggers: structure.triggers.len(),
            },
            &self.expanded_sections,
            cx,
            theme,
        );
        h_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .gap(px(10.0))
            .child(outline)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        div()
                            .id("table-properties-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.vertical_scroll)
                            .child(sections),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(14.0))
                            .child(
                                Scrollbar::vertical(&self.vertical_scroll)
                                    .id("table-properties-scrollbar")
                                    .scrollbar_show(ScrollbarShow::Always),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn toggle_section(
        &mut self,
        section: TablePropertiesSection,
        cx: &mut Context<Self>,
    ) {
        if !self.expanded_sections.remove(&section) {
            self.expanded_sections.insert(section);
        }
        cx.notify();
    }

    fn render_loaded(
        &self,
        structure: &LoadedTableStructure,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = match self.active_tab {
            TablePropertiesTab::Structure => self.render_structure(structure, theme, cx),
            TablePropertiesTab::Ddl => render_ddl(
                self.ddl_loading,
                self.ddl_text.clone(),
                self.ddl_error.clone(),
                &self.ddl_vertical_scroll,
                &self.ddl_horizontal_scroll,
                theme,
            ),
        };
        v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .gap(px(8.0))
            .child(self.render_tabs(theme, cx))
            .child(body)
            .into_any_element()
    }
}

impl Render for TablePropertiesDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = if self.loading && self.structure.is_none() {
            v_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().small())
                .child("正在读取表结构元数据…")
                .into_any_element()
        } else if let Some(structure) = &self.structure {
            self.render_loaded(structure, &theme, cx)
        } else {
            v_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("暂时没有可显示的表结构")
                .into_any_element()
        };

        v_flex()
            .w_full()
            .h(px(VIEW_HEIGHT))
            .min_h_0()
            .gap(px(8.0))
            .child(self.render_toolbar(cx))
            .child(content)
    }
}

async fn load_table_structure(
    service: Arc<ConnectionService>,
    connection: ConnectionConfig,
    schema: String,
    table: String,
) -> LoadedTableStructure {
    let (columns_result, indexes_result, foreign_keys_result, triggers_result) = futures::join!(
        service.list_columns(&connection, &schema, &table),
        service.list_indexes(&connection, &schema, &table),
        service.list_foreign_keys(&connection, &schema, &table),
        service.list_triggers(&connection, &schema, &table),
    );
    let (columns, column_warning) = keep_metadata("列", columns_result);
    let (indexes, index_warning) = keep_metadata("索引", indexes_result);
    let (foreign_keys, foreign_key_warning) = keep_metadata("外键", foreign_keys_result);
    let (triggers, trigger_warning) = keep_metadata("触发器", triggers_result);
    let mut warnings = Vec::new();
    for warning in [
        column_warning,
        index_warning,
        foreign_key_warning,
        trigger_warning,
    ]
    .into_iter()
    .flatten()
    {
        warnings.push(warning);
    }
    LoadedTableStructure {
        columns,
        indexes,
        foreign_keys,
        triggers,
        warnings,
    }
}

/// Loads the database-native definition used by the read-only DDL tab.
/// The result keeps the driver's original SQL text so users can inspect or copy it without edits.
async fn load_table_ddl(
    service: Arc<ConnectionService>,
    connection: ConnectionConfig,
    schema: String,
    table: String,
    is_view: bool,
) -> anyhow::Result<String> {
    let sql = ramag_domain::entities::build_ddl_query(connection.driver, &schema, &table, is_view);
    if sql.is_empty() {
        return Err(anyhow::anyhow!("当前数据库类型不支持 DDL 查看"));
    }
    let result = service.execute(&connection, &Query::new(sql)).await?;
    result
        .rows
        .first()
        .and_then(|row| row.values.iter().rev().find_map(value_as_ddl))
        .filter(|ddl| !ddl.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("数据库未返回 {schema}.{table} 的定义"))
}

fn value_as_ddl(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::Json(value) => Some(value.to_string()),
        _ => None,
    }
}

fn keep_metadata<T>(
    label: &str,
    result: ramag_domain::error::Result<Vec<T>>,
) -> (Vec<T>, Option<String>) {
    match result {
        Ok(items) => (items, None),
        Err(error) => (Vec::new(), Some(format!("{label}加载失败：{error}"))),
    }
}
