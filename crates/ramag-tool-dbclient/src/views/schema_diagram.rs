//! 基于 SQL 元数据的只读 Schema Diagram 预览。

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, Theme,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};
use ramag_app::ConnectionService;
use ramag_domain::entities::ConnectionConfig;

const MAX_DIAGRAM_TABLES: usize = 128;
const MAX_DIAGRAM_COLUMNS: usize = 64;
const MAX_DIAGRAM_RELATIONS_PER_TABLE: usize = 64;
const MAX_VISIBLE_COLUMNS: usize = 18;
const GRID_COLUMNS: usize = 4;
const CARD_WIDTH: f32 = 286.0;
const CARD_GAP: f32 = 12.0;

#[derive(Clone, Debug)]
struct DiagramColumn {
    name: String,
    raw_type: String,
    nullable: bool,
    primary: bool,
}

#[derive(Clone, Debug)]
struct DiagramRelation {
    source_table: String,
    name: String,
    columns: Vec<String>,
    ref_schema: String,
    ref_table: String,
    ref_columns: Vec<String>,
}

#[derive(Clone, Debug)]
struct DiagramTable {
    name: String,
    is_view: bool,
    comment: Option<String>,
    columns: Vec<DiagramColumn>,
    relations: Vec<DiagramRelation>,
    metadata_error: Option<String>,
}

struct LoadedDiagram {
    tables: Vec<DiagramTable>,
    relations: Vec<DiagramRelation>,
    omitted_tables: usize,
}

pub struct SchemaDiagramPanel {
    service: Arc<ConnectionService>,
    connection: ConnectionConfig,
    schema: String,
    search: Entity<InputState>,
    tables: Vec<DiagramTable>,
    relations: Vec<DiagramRelation>,
    omitted_tables: usize,
    loading: bool,
    error: Option<String>,
    request_generation: u64,
    vertical_scroll: ScrollHandle,
    horizontal_scroll: ScrollHandle,
}

impl SchemaDiagramPanel {
    pub fn new(
        service: Arc<ConnectionService>,
        connection: ConnectionConfig,
        schema: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("筛选表名 / 列名"));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();

        let mut this = Self {
            service,
            connection,
            schema,
            search,
            tables: Vec::new(),
            relations: Vec::new(),
            omitted_tables: 0,
            loading: false,
            error: None,
            request_generation: 0,
            vertical_scroll: ScrollHandle::new(),
            horizontal_scroll: ScrollHandle::new(),
        };
        this.refresh(cx);
        this
    }

    /// Refreshes all nodes and edges from the selected schema's metadata.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let request_generation = self.request_generation;
        let service = self.service.clone();
        let connection = self.connection.clone();
        let schema = self.schema.clone();
        self.loading = true;
        self.error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = load_diagram(service, connection.clone(), schema.clone()).await;
            let _ = this.update(cx, |this, cx| {
                if this.request_generation != request_generation
                    || this.connection.id != connection.id
                    || this.schema != schema
                {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(loaded) => {
                        this.tables = loaded.tables;
                        this.relations = loaded.relations;
                        this.omitted_tables = loaded.omitted_tables;
                    }
                    Err(error) => {
                        this.tables.clear();
                        this.relations.clear();
                        this.omitted_tables = 0;
                        this.error = Some(error);
                    }
                }
                this.vertical_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                this.horizontal_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                cx.notify();
            });
        })
        .detach();
    }

    fn search_query(&self, cx: &gpui::App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    /// Filters table nodes by name, column name, or raw database type.
    fn visible_tables<'a>(&'a self, query: &str) -> Vec<&'a DiagramTable> {
        self.tables
            .iter()
            .filter(|table| {
                query.is_empty()
                    || table.name.to_lowercase().contains(query)
                    || table.columns.iter().any(|column| {
                        column.name.to_lowercase().contains(query)
                            || column.raw_type.to_lowercase().contains(query)
                    })
            })
            .collect()
    }

    /// Renders one bounded table node, including columns and outgoing foreign keys.
    fn render_table_card(
        &self,
        table: &DiagramTable,
        theme: &Theme,
        _cx: &Context<Self>,
    ) -> impl IntoElement {
        let border = theme.border;
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let danger = theme.danger;
        let secondary = theme.secondary;
        let card_bg = theme.background;
        let icon = if table.is_view {
            IconName::Frame
        } else {
            IconName::MemoryStick
        };

        let mut card = v_flex()
            .w(px(CARD_WIDTH))
            .flex_none()
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .bg(card_bg)
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .py(px(7.0))
                    .bg(secondary)
                    .border_b_1()
                    .border_color(border)
                    .child(Icon::new(icon).small().text_color(muted_fg))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(table.name.clone()),
                    )
                    .when(table.is_view, |header| {
                        header.child(
                            div()
                                .px(px(4.0))
                                .py(px(1.0))
                                .rounded(px(3.0))
                                .text_xs()
                                .text_color(muted_fg)
                                .child("视图"),
                        )
                    }),
            );

        if let Some(comment) = table
            .comment
            .as_deref()
            .filter(|comment| !comment.is_empty())
        {
            card = card.child(
                div()
                    .w_full()
                    .px_2()
                    .pt(px(5.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(comment.to_string()),
            );
        }

        let columns = table.columns.iter().take(MAX_VISIBLE_COLUMNS);
        for column in columns {
            let marker = if column.primary { "PK" } else { "" };
            let marker_color = if column.primary {
                theme.warning
            } else {
                muted_fg
            };
            card = card.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py(px(2.0))
                    .child(
                        div()
                            .w(px(22.0))
                            .flex_none()
                            .text_xs()
                            .text_color(marker_color)
                            .child(marker),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(column.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(muted_fg)
                            .child(column.raw_type.clone()),
                    )
                    .when(column.nullable, |row| {
                        row.child(div().text_xs().text_color(muted_fg).child("?"))
                    }),
            );
        }
        if table.columns.len() > MAX_VISIBLE_COLUMNS {
            card = card.child(
                div()
                    .px_2()
                    .py(px(3.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!(
                        "… 还有 {} 列",
                        table.columns.len() - MAX_VISIBLE_COLUMNS
                    )),
            );
        }

        if let Some(error) = &table.metadata_error {
            card = card.child(
                div()
                    .w_full()
                    .px_2()
                    .py(px(5.0))
                    .text_xs()
                    .text_color(danger)
                    .child(format!("元数据不完整：{error}")),
            );
        }

        if !table.relations.is_empty() {
            card = card.child(
                div()
                    .w_full()
                    .mt(px(4.0))
                    .pt(px(5.0))
                    .px_2()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("外键关系 ({})", table.relations.len())),
            );
            for relation in table.relations.iter().take(4) {
                card = card.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py(px(2.0))
                        .child(
                            Icon::new(IconName::ArrowRight)
                                .xsmall()
                                .text_color(muted_fg),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(fg)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(format!(
                                    "{} → {}.{}",
                                    relation.name, relation.ref_schema, relation.ref_table
                                )),
                        ),
                );
            }
            if table.relations.len() > 4 {
                card = card.child(
                    div()
                        .px_2()
                        .py(px(2.0))
                        .text_xs()
                        .text_color(muted_fg)
                        .child(format!("… 还有 {} 个关系", table.relations.len() - 4)),
                );
            }
        }

        card
    }

    /// Renders the relation list below the grid so edges remain readable without a canvas.
    fn render_relation_summary(
        &self,
        relations: &[DiagramRelation],
        visible_table_names: &HashSet<&str>,
        theme: &Theme,
    ) -> impl IntoElement {
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let fg = theme.foreground;
        let visible_relations: Vec<&DiagramRelation> = relations
            .iter()
            .filter(|relation| visible_table_names.contains(relation.source_table.as_str()))
            .collect();
        let mut section = v_flex()
            .w_full()
            .mt(px(16.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .px_2()
                    .py(px(6.0))
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child(format!("关系 ({})", visible_relations.len())),
                    ),
            );
        for relation in visible_relations.iter().take(64) {
            section = section.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(4.0))
                    .child(
                        div()
                            .w(px(190.0))
                            .flex_none()
                            .text_xs()
                            .text_color(fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("{}.{}", relation.source_table, relation.name)),
                    )
                    .child(
                        Icon::new(IconName::ArrowRight)
                            .xsmall()
                            .text_color(muted_fg),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted_fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!(
                                "{}.{} ({}) → ({})",
                                relation.ref_schema,
                                relation.ref_table,
                                relation.columns.join(", "),
                                relation.ref_columns.join(", ")
                            )),
                    ),
            );
        }
        if visible_relations.len() > 64 {
            section = section.child(
                div()
                    .px_2()
                    .py(px(4.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("… 还有 {} 个关系", visible_relations.len() - 64)),
            );
        }
        section
    }
}

impl Render for SchemaDiagramPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let query = self.search_query(cx);
        let visible_tables = self.visible_tables(&query);
        let visible_table_names: HashSet<&str> = visible_tables
            .iter()
            .map(|table| table.name.as_str())
            .collect();
        let filtered_count = visible_tables.len();
        let table_count = self.tables.len();
        let content_columns = filtered_count.clamp(1, GRID_COLUMNS);
        let content_width = (content_columns as f32 * CARD_WIDTH
            + content_columns.saturating_sub(1) as f32 * CARD_GAP
            + 24.0)
            .max(640.0);

        let mut grid = v_flex().w(px(content_width)).gap(px(CARD_GAP));
        for row in visible_tables.chunks(GRID_COLUMNS) {
            let mut row_element = h_flex().w_full().items_start().gap(px(CARD_GAP));
            for table in row {
                row_element = row_element.child(self.render_table_card(table, theme, cx));
            }
            grid = grid.child(row_element);
        }
        if visible_tables.is_empty() && !self.loading && self.error.is_none() {
            grid = grid.child(
                div()
                    .w_full()
                    .py(px(40.0))
                    .text_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if query.is_empty() {
                        "该 Schema 没有可展示的表"
                    } else {
                        "没有匹配的表或列"
                    }),
            );
        }
        let grid =
            grid.child(self.render_relation_summary(&self.relations, &visible_table_names, theme));

        let mut toolbar = h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .py(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("Schema Diagram · {}", self.schema)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search).small().bordered(false)),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{filtered_count}/{table_count} 表")),
            )
            .child(
                ramag_ui::clickable_button("schema-diagram-refresh")
                    .ghost()
                    .small()
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("刷新元数据")
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );
        if self.omitted_tables > 0 {
            toolbar = toolbar.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(format!("已省略 {} 张表", self.omitted_tables)),
            );
        }

        let content = if let Some(error) = &self.error {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_xs()
                .text_color(theme.danger)
                .child(format!("加载 Schema Diagram 失败：{error}"))
                .child(
                    ramag_ui::clickable_button("schema-diagram-retry")
                        .small()
                        .label("重试")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
                .into_any_element()
        } else if self.loading && self.tables.is_empty() {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("正在加载表、列和外键元数据…")
                .into_any_element()
        } else {
            div()
                .id("schema-diagram-viewport")
                .relative()
                .size_full()
                .min_h_0()
                .child(
                    div()
                        .id("schema-diagram-horizontal-scroll")
                        .size_full()
                        .overflow_x_scroll()
                        .track_scroll(&self.horizontal_scroll)
                        .child(
                            div()
                                .id("schema-diagram-vertical-scroll")
                                .w(px(content_width))
                                .h_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.vertical_scroll)
                                .child(grid),
                        ),
                )
                .child(
                    div()
                        .id("schema-diagram-vertical-scrollbar")
                        .absolute()
                        .top_0()
                        .bottom(px(16.0))
                        .right_0()
                        .w(px(16.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::vertical(&self.vertical_scroll)
                                .id("schema-diagram-vertical-scrollbar-control")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                )
                .child(
                    div()
                        .id("schema-diagram-horizontal-scrollbar")
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(16.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::horizontal(&self.horizontal_scroll)
                                .id("schema-diagram-horizontal-scrollbar-control")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                )
                .into_any_element()
        };

        v_flex()
            .w_full()
            .h(px(700.0))
            .min_h_0()
            .child(toolbar)
            .child(content)
    }
}

/// Loads bounded table metadata concurrently and returns nodes plus metadata-backed edges.
async fn load_diagram(
    service: Arc<ConnectionService>,
    connection: ConnectionConfig,
    schema: String,
) -> Result<LoadedDiagram, String> {
    let tables = service
        .list_tables(&connection, &schema)
        .await
        .map_err(|error| error.to_string())?;
    let omitted_tables = tables.len().saturating_sub(MAX_DIAGRAM_TABLES);
    let tables = tables.into_iter().take(MAX_DIAGRAM_TABLES);
    let futures = tables.map(|table| {
        let service = service.clone();
        let connection = connection.clone();
        let schema = schema.clone();
        async move {
            let table_name = table.name.clone();
            let (columns_result, foreign_keys_result) = futures::join!(
                service.list_columns(&connection, &schema, &table_name),
                service.list_foreign_keys(&connection, &schema, &table_name)
            );
            let mut metadata_errors = Vec::new();
            let columns = match columns_result {
                Ok(columns) => columns,
                Err(error) => {
                    metadata_errors.push(format!("列：{error}"));
                    Vec::new()
                }
            };
            let foreign_keys = match foreign_keys_result {
                Ok(keys) => keys,
                Err(error) => {
                    metadata_errors.push(format!("外键：{error}"));
                    Vec::new()
                }
            };
            let relations: Vec<DiagramRelation> = foreign_keys
                .into_iter()
                .take(MAX_DIAGRAM_RELATIONS_PER_TABLE)
                .map(|foreign_key| DiagramRelation {
                    source_table: table.name.clone(),
                    name: foreign_key.name,
                    columns: foreign_key.columns,
                    ref_schema: foreign_key.ref_schema,
                    ref_table: foreign_key.ref_table,
                    ref_columns: foreign_key.ref_columns,
                })
                .collect();
            let columns = columns
                .into_iter()
                .take(MAX_DIAGRAM_COLUMNS)
                .map(|column| DiagramColumn {
                    name: column.name,
                    raw_type: column.data_type.raw_type,
                    nullable: column.nullable,
                    primary: column.is_primary_key,
                })
                .collect();
            DiagramTable {
                name: table.name,
                is_view: table.is_view,
                comment: table.comment,
                columns,
                relations,
                metadata_error: (!metadata_errors.is_empty()).then(|| metadata_errors.join("；")),
            }
        }
    });
    let tables: Vec<DiagramTable> = futures::future::join_all(futures).await;
    let relations = tables
        .iter()
        .flat_map(|table| table.relations.iter().cloned())
        .collect();
    Ok(LoadedDiagram {
        tables,
        relations,
        omitted_tables,
    })
}

#[cfg(test)]
mod tests {
    use super::{CARD_GAP, CARD_WIDTH, GRID_COLUMNS};

    #[test]
    fn diagram_width_keeps_a_wide_schema_scrollable() {
        let visible_tables = 9usize;
        let columns = visible_tables.clamp(1, GRID_COLUMNS);
        let width =
            (columns as f32 * CARD_WIDTH + columns.saturating_sub(1) as f32 * CARD_GAP + 24.0)
                .max(640.0);
        assert!(width > 640.0);
        assert_eq!(columns, GRID_COLUMNS);
    }

    #[test]
    fn empty_diagram_still_has_a_stable_minimum_canvas() {
        let columns = 0usize.clamp(1, GRID_COLUMNS);
        let width =
            (columns as f32 * CARD_WIDTH + columns.saturating_sub(1) as f32 * CARD_GAP + 24.0)
                .max(640.0);
        assert_eq!(columns, 1);
        assert_eq!(width, 640.0);
    }
}
