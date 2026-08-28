//! 单表结构只读视图：按 DataGrip 风格分组展示列、键、索引、外键和触发器。

use std::sync::Arc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Render, ScrollHandle, Styled, Window, div,
    prelude::*, px,
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
use ramag_domain::entities::{Column, ConnectionConfig, ForeignKey, Index, Trigger};

mod render;

use self::render::{
    is_key, render_columns, render_foreign_keys, render_indexes, render_outline, render_section,
    render_triggers, render_warnings,
};

const VIEW_HEIGHT: f32 = 650.0;

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
    request_generation: u64,
    vertical_scroll: ScrollHandle,
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
            request_generation: 0,
            vertical_scroll: ScrollHandle::new(),
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
        self.loading = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let structure = load_table_structure(service, connection.clone(), schema, table).await;
            let _ = this.update(cx, |this, cx| {
                if this.request_generation != request_generation
                    || this.connection.id != connection.id
                {
                    return;
                }
                this.structure = Some(structure);
                this.loading = false;
                this.vertical_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                cx.notify();
            });
        })
        .detach();
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
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

    fn render_loaded(&self, structure: &LoadedTableStructure, theme: &Theme) -> AnyElement {
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
        sections = sections
            .child(render_section(
                "列",
                structure.columns.len(),
                IconName::MemoryStick,
                render_columns(&structure.columns, theme),
                theme,
            ))
            .child(render_section(
                "键",
                keys.len(),
                IconName::File,
                render_indexes(&keys, theme, true),
                theme,
            ))
            .child(render_section(
                "索引",
                indexes.len(),
                IconName::File,
                render_indexes(&indexes, theme, false),
                theme,
            ))
            .child(render_section(
                "外键",
                structure.foreign_keys.len(),
                IconName::ArrowRight,
                render_foreign_keys(&structure.foreign_keys, theme),
                theme,
            ))
            .child(render_section(
                "触发器",
                structure.triggers.len(),
                IconName::Network,
                render_triggers(&structure.triggers, theme),
                theme,
            ));

        let outline = render_outline(
            structure.columns.len(),
            keys.len(),
            indexes.len(),
            structure.foreign_keys.len(),
            structure.triggers.len(),
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
}

impl Render for TablePropertiesDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
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
            self.render_loaded(structure, theme)
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

fn keep_metadata<T>(
    label: &str,
    result: ramag_domain::error::Result<Vec<T>>,
) -> (Vec<T>, Option<String>) {
    match result {
        Ok(items) => (items, None),
        Err(error) => (Vec::new(), Some(format!("{label}加载失败：{error}"))),
    }
}
