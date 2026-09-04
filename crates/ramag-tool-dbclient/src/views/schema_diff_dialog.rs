//! 两张 SQL 表的只读结构对比对话框。

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, ScrollHandle, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, Theme,
    button::ButtonVariants as _,
    h_flex,
    notification::Notification,
    scroll::{Scrollbar, ScrollbarShow},
    spinner::Spinner,
    v_flex,
};
use ramag_app::ConnectionService;
use ramag_domain::entities::ConnectionConfig;

use super::schema_diff::{
    MetadataDiffKind, MetadataDiffLine, MetadataDiffSection, TableMetadata, build_table_diff,
    format_table_diff,
};
use super::schema_migration::build_migration_script;

mod migration;
use migration::MigrationApprovalRecord;

const DIFF_VIEW_WIDTH: f32 = 980.0;
const DIFF_VIEW_HEIGHT: f32 = 520.0;

#[derive(Clone, Debug)]
struct LoadedMetadata {
    metadata: TableMetadata,
    warnings: Vec<String>,
}

pub(crate) struct SchemaDiffDialog {
    service: Arc<ConnectionService>,
    source_connection: ConnectionConfig,
    target_connection: ConnectionConfig,
    source_schema: String,
    source_table: String,
    target_schema: String,
    target_table: String,
    source: Option<LoadedMetadata>,
    target: Option<LoadedMetadata>,
    loading: bool,
    request_generation: u64,
    error: Option<String>,
    vertical_scroll: ScrollHandle,
    horizontal_scroll: ScrollHandle,
    migration_vertical_scroll: ScrollHandle,
    migration_horizontal_scroll: ScrollHandle,
    migration_visible: bool,
    saving_migration: bool,
    executing_migration: bool,
    migration_execution_generation: u64,
    migration_approvals: Vec<MigrationApprovalRecord>,
    pending_notification: Option<Notification>,
}

impl SchemaDiffDialog {
    /// 初始化空的对比状态，并立即启动一次列、索引和外键元数据读取。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        service: Arc<ConnectionService>,
        source_connection: ConnectionConfig,
        target_connection: ConnectionConfig,
        source_schema: String,
        source_table: String,
        target_schema: String,
        target_table: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            service,
            source_connection,
            target_connection,
            source_schema,
            source_table,
            target_schema,
            target_table,
            source: None,
            target: None,
            loading: false,
            request_generation: 0,
            error: None,
            vertical_scroll: ScrollHandle::new(),
            horizontal_scroll: ScrollHandle::new(),
            migration_vertical_scroll: ScrollHandle::new(),
            migration_horizontal_scroll: ScrollHandle::new(),
            migration_visible: false,
            saving_migration: false,
            executing_migration: false,
            migration_execution_generation: 0,
            migration_approvals: Vec::new(),
            pending_notification: None,
        };
        this.refresh(cx);
        migration::load_migration_approvals(cx);
        this
    }

    /// 读取两张表的列、索引和外键；单类元数据失败时保留其他类别并显示警告。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.executing_migration {
            return;
        }
        self.request_generation = self.request_generation.wrapping_add(1);
        let request_generation = self.request_generation;
        let service = self.service.clone();
        let source_connection = self.source_connection.clone();
        let target_connection = self.target_connection.clone();
        let source_schema = self.source_schema.clone();
        let source_table = self.source_table.clone();
        let target_schema = self.target_schema.clone();
        let target_table = self.target_table.clone();
        self.loading = true;
        self.error = None;
        self.migration_visible = false;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let (source, target) = futures::join!(
                load_table_metadata(
                    service.clone(),
                    source_connection.clone(),
                    source_schema.clone(),
                    source_table.clone(),
                ),
                load_table_metadata(
                    service,
                    target_connection.clone(),
                    target_schema.clone(),
                    target_table.clone(),
                ),
            );
            let _ = this.update(cx, |this, cx| {
                if this.request_generation != request_generation
                    || this.source_connection.id != source_connection.id
                    || this.target_connection.id != target_connection.id
                    || this.source_schema != source_schema
                    || this.source_table != source_table
                    || this.target_schema != target_schema
                    || this.target_table != target_table
                {
                    return;
                }
                this.loading = false;
                this.source = Some(source);
                this.target = Some(target);
                this.vertical_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                this.horizontal_scroll
                    .set_offset(gpui::Point::new(px(0.0), px(0.0)));
                cx.notify();
            });
        })
        .detach();
    }

    fn render_diff_line(&self, line: &MetadataDiffLine, theme: &Theme) -> impl IntoElement {
        let (prefix, color, background) = match line.kind {
            MetadataDiffKind::Context => (" ", theme.muted_foreground, theme.background),
            MetadataDiffKind::Added => ("+", theme.success, theme.success.opacity(0.08)),
            MetadataDiffKind::Removed => ("-", theme.danger, theme.danger.opacity(0.08)),
        };
        h_flex()
            .w_full()
            .items_start()
            .gap(px(6.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded_sm()
            .bg(background)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .child(div().w(px(14.0)).text_color(color).child(prefix))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(color)
                    .child(line.text.clone()),
            )
    }

    fn render_section(&self, section: &MetadataDiffSection, theme: &Theme) -> impl IntoElement {
        let mut body = v_flex()
            .w_full()
            .gap(px(2.0))
            .p(px(8.0))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(section.title),
            );
        if section.lines.is_empty() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("无差异"),
            );
        } else {
            for line in &section.lines {
                body = body.child(self.render_diff_line(line, theme));
            }
        }
        body
    }

    fn render_warnings(
        &self,
        source: &LoadedMetadata,
        target: &LoadedMetadata,
        theme: &Theme,
    ) -> Option<gpui::AnyElement> {
        let warnings = source
            .warnings
            .iter()
            .map(|warning| format!("源表：{warning}"))
            .chain(
                target
                    .warnings
                    .iter()
                    .map(|warning| format!("目标表：{warning}")),
            )
            .collect::<Vec<_>>();
        (!warnings.is_empty()).then(|| {
            v_flex()
                .w_full()
                .gap(px(2.0))
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(theme.warning.opacity(0.08))
                .children(
                    warnings
                        .into_iter()
                        .map(|warning| div().text_xs().text_color(theme.warning).child(warning)),
                )
                .into_any_element()
        })
    }
}

impl Render for SchemaDiffDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            ramag_ui::push_responsive_notification(window, notification, cx);
        }
        let theme = cx.theme().clone();
        let theme = &theme;
        let sections = self
            .source
            .as_ref()
            .zip(self.target.as_ref())
            .map(|(source, target)| build_table_diff(&source.metadata, &target.metadata));
        let has_sections = sections.is_some();
        let metadata_complete =
            self.source
                .as_ref()
                .zip(self.target.as_ref())
                .is_some_and(|(source, target)| {
                    source.warnings.is_empty() && target.warnings.is_empty()
                });
        let migration = self
            .source
            .as_ref()
            .zip(self.target.as_ref())
            .map(|(source, target)| {
                if metadata_complete {
                    build_migration_script(
                        self.target_connection.driver,
                        &self.source_schema,
                        &self.source_table,
                        &self.target_schema,
                        &self.target_table,
                        &source.metadata,
                        &target.metadata,
                    )
                } else {
                    Err("源表或目标表的元数据未完整加载，无法生成迁移 SQL；请刷新后重试".into())
                }
            });
        let copy_text = sections
            .as_deref()
            .map(format_table_diff)
            .unwrap_or_default();
        let warning_panel = self
            .source
            .as_ref()
            .zip(self.target.as_ref())
            .and_then(|(source, target)| self.render_warnings(source, target, theme));
        let body = if self.migration_visible {
            self.render_migration_panel(migration.as_ref(), theme, cx)
        } else if self.loading && sections.is_none() {
            v_flex()
                .h(px(DIFF_VIEW_HEIGHT))
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().small())
                .child("正在加载两张表的列、索引和外键…")
                .into_any_element()
        } else if let Some(error) = &self.error {
            v_flex()
                .h(px(DIFF_VIEW_HEIGHT))
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .text_xs()
                .text_color(theme.danger)
                .child(error.clone())
                .into_any_element()
        } else if let Some(sections) = sections {
            let mut content = v_flex().w(px(DIFF_VIEW_WIDTH)).gap(px(8.0));
            for section in &sections {
                content = content.child(self.render_section(section, theme));
            }
            div()
                .relative()
                .h(px(DIFF_VIEW_HEIGHT))
                .w_full()
                .child(
                    div()
                        .id("schema-diff-horizontal-scroll")
                        .size_full()
                        .overflow_x_scroll()
                        .track_scroll(&self.horizontal_scroll)
                        .child(
                            div()
                                .id("schema-diff-vertical-scroll")
                                .w(px(DIFF_VIEW_WIDTH))
                                .h_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.vertical_scroll)
                                .child(content),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom(px(16.0))
                        .right_0()
                        .w(px(16.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::vertical(&self.vertical_scroll)
                                .id("schema-diff-vertical-scrollbar")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(16.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::horizontal(&self.horizontal_scroll)
                                .id("schema-diff-horizontal-scrollbar")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .h(px(DIFF_VIEW_HEIGHT))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("暂无可比较的元数据")
                .into_any_element()
        };

        let toolbar = ramag_ui::responsive_toolbar()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("{} · {}", self.source_table, self.target_table)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "源：{} / {}.{} · 目标：{} / {}.{}",
                                self.source_connection.name,
                                self.source_schema,
                                self.source_table,
                                self.target_connection.name,
                                self.target_schema,
                                self.target_table
                            )),
                    ),
            )
            .child(
                ramag_ui::clickable_button("schema-diff-migration")
                    .ghost()
                    .small()
                    .icon(if self.migration_visible {
                        IconName::ArrowLeft
                    } else {
                        IconName::File
                    })
                    .tooltip(if self.migration_visible {
                        "返回结构差异"
                    } else if !metadata_complete {
                        "元数据未完整加载，无法预览迁移 SQL"
                    } else {
                        "预览迁移 SQL"
                    })
                    .disabled(
                        self.loading
                            || self.executing_migration
                            || !has_sections
                            || !metadata_complete,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_migration(cx))),
            )
            .child(
                ramag_ui::clickable_button("schema-diff-copy")
                    .ghost()
                    .small()
                    .icon(IconName::Copy)
                    .tooltip("复制结构差异")
                    .disabled(self.loading || !has_sections)
                    .on_click(move |_: &ClickEvent, window, app| {
                        ramag_ui::copy_text_with_notification(copy_text.clone(), window, app);
                    }),
            )
            .child(
                ramag_ui::clickable_button("schema-diff-refresh")
                    .ghost()
                    .small()
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("重新加载元数据")
                    .disabled(self.loading || self.executing_migration)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );

        v_flex()
            .w_full()
            .gap(px(8.0))
            .child(toolbar)
            .when_some(warning_panel, |content, panel| content.child(panel))
            .child(body)
    }
}

async fn load_table_metadata(
    service: Arc<ConnectionService>,
    connection: ConnectionConfig,
    schema: String,
    table: String,
) -> LoadedMetadata {
    let (columns_result, indexes_result, foreign_keys_result) = futures::join!(
        service.list_columns(&connection, &schema, &table),
        service.list_indexes(&connection, &schema, &table),
        service.list_foreign_keys(&connection, &schema, &table),
    );
    let mut warnings = Vec::new();
    let columns = match columns_result {
        Ok(columns) => columns,
        Err(error) => {
            warnings.push(format!("列加载失败：{error}"));
            Vec::new()
        }
    };
    let indexes = match indexes_result {
        Ok(indexes) => indexes,
        Err(error) => {
            warnings.push(format!("索引加载失败：{error}"));
            Vec::new()
        }
    };
    let foreign_keys = match foreign_keys_result {
        Ok(foreign_keys) => foreign_keys,
        Err(error) => {
            warnings.push(format!("外键加载失败：{error}"));
            Vec::new()
        }
    };
    LoadedMetadata {
        metadata: TableMetadata {
            columns,
            indexes,
            foreign_keys,
        },
        warnings,
    }
}
