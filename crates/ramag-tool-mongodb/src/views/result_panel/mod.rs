mod cell;
mod drill;
mod edit;
mod export;
mod filter;
mod flatten;
mod helpers;
mod ops;
mod render;
mod row;
mod row_search;
mod row_view;
mod table;
mod toolbar;

#[cfg(test)]
mod render_test;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement, Point, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled, UniformListScrollHandle,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    h_flex, input::InputState, v_flex,
};
use parking_lot::RwLock;
use ramag_app::MongoService;
use ramag_domain::entities::{ConflictPolicy, ConnectionConfig, MongoQueryResult};
use ramag_ui::{AxisScrollGesture, ResultMemoryLease, ResultMemoryUpdate};

pub use flatten::FlatTable;

use crate::views::inline_text_preview;
use filter::{ParsedFilter, classify_filter, column_indices_for, row_indices_for_cancellable};
use helpers::{bounded_cell_dialog_text, memory_notice, pretty_cell_value};
use row_search::{RowFilter, RowSearchBlocker, RowSearchState};
pub(crate) use row_search::{RowSearchConversionStatus, RowSearchMode};

/// 列补全最大嵌套深度。
const PATH_COMPLETION_DEPTH: usize = 5;
/// 行过滤防抖，避免按键时反复扫描大表。
const ROW_VIEW_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(180);

pub struct ResultPanel {
    pub(crate) result: Option<MongoQueryResult>,
    /// 当前层文档由此共享，避免渲染时复制。
    pub(crate) docs_arc: Option<Arc<Vec<serde_json::Value>>>,
    pub(crate) error: Option<String>,
    /// 当前结果的内存提示。
    memory_notice: Option<String>,
    pub(crate) running: bool,
    pagination: Option<MongoResultPagination>,
    pub(crate) table: Option<Arc<FlatTable>>,
    pub(crate) table_building: bool,
    /// 表格构建代际，防止旧任务回包生效。
    table_build_seq: u64,
    table_build_cancel: Option<Arc<AtomicBool>>,
    pub(crate) column_filter: Entity<InputState>,
    pub(crate) row_filter: Entity<InputState>,
    row_search: RowSearchState,
    pub(crate) uniform_scroll: UniformListScrollHandle,
    pub(crate) h_scroll: ScrollHandle,
    /// 表格双轴手势状态。
    scroll_gesture: AxisScrollGesture,
    pub(crate) column_completion_source: Arc<RwLock<Vec<String>>>,
    pub(crate) service: Option<Arc<MongoService>>,
    pub(crate) config: Option<ConnectionConfig>,
    pub(crate) database: String,
    pub(crate) target_collection: Option<String>,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pub(crate) pending_notification: Option<gpui_component::notification::Notification>,
    /// DML 防重入；失败时保留弹框输入。
    pub(super) doc_dml_busy: bool,
    pub(super) exporting: bool,
    /// 仅成功后关闭 DML 弹框。
    pub(super) pending_close_dialog: bool,
    pub(crate) selected_rows: BTreeSet<usize>,
    /// 选择代际与可见行交集缓存，避免重复扫描。
    selection_revision: u64,
    visible_selection_cache: Option<VisibleSelectionCache>,
    /// 下钻栈；深度大于 1 时只读并显示面包屑。
    pub(crate) drill_stack: Vec<drill::DrillLevel>,
    /// 下钻换表后用路径检测排序失配。
    pub(crate) sort_by: Option<(String, SortDir)>,
    /// 行过滤和排序缓存，避免重复扫描矩阵。
    row_view_cache: Option<RowViewCache>,
    /// 行视图构建期间禁用依赖当前视图的操作。
    pub(crate) row_view_building: bool,
    /// 行视图请求代际，防止旧回包覆盖。
    row_view_request_seq: u64,
    row_view_cancel: Option<Arc<AtomicBool>>,
    /// 行视图构建失败信息；条件变化时清除。
    pub(crate) row_view_error: Option<String>,
    /// 当前标签的结果内存登记。
    result_memory: Option<ResultMemoryLease>,
    _subscriptions: Vec<gpui::Subscription>,
}

#[derive(Clone, Debug)]
pub enum ResultEvent {
    Refresh,
    /// 仅取消客户端等待，服务端操作仍可能继续。
    Cancel,
    PageRequested(usize),
    CollectionImportRequested {
        db: String,
        collection: String,
        policy: ConflictPolicy,
        files: Vec<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MongoResultPagination {
    pub page: usize,
    pub page_size: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowViewKey {
    generation: u64,
    filter: RowFilter,
    sort_by: Option<(String, SortDir)>,
}

struct RowViewCache {
    key: RowViewKey,
    indices: Arc<Vec<usize>>,
}

struct VisibleSelectionCache {
    rows: Arc<Vec<usize>>,
    selection_revision: u64,
    visible_selected: usize,
}

fn build_row_view_indices(
    table: &FlatTable,
    key: &RowViewKey,
    cancelled: &AtomicBool,
) -> Option<Arc<Vec<usize>>> {
    let mut indices = row_indices_for_cancellable(table, &key.filter, Some(cancelled))
        .ok()?
        .unwrap_or_else(|| (0..table.rows.len()).collect());
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    if let Some((sort_path, direction)) = &key.sort_by
        && let Some(column_index) = table
            .columns
            .iter()
            .position(|column| column.path == *sort_path)
    {
        let numeric = matches!(
            table.columns[column_index].kind,
            "int" | "long" | "double" | "decimal"
        );
        table::sort_row_indices(table, column_index, numeric, *direction, &mut indices);
    }
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    Some(Arc::new(indices))
}

fn visible_selection_count(selected: &BTreeSet<usize>, visible: &[usize]) -> usize {
    visible.iter().filter(|row| selected.contains(row)).count()
}

impl EventEmitter<ResultEvent> for ResultPanel {}

impl ResultPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let column_completion_source: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
        let provider = crate::completion::ColumnFilterCompletionProvider::new_rc(
            column_completion_source.clone(),
        );
        let column_filter = cx.new(|cx| {
            let mut state = ramag_ui::bounded_search_input(window, cx)
                .placeholder("过滤列（逗号分隔；填路径可钻取）");
            state.lsp.completion_provider = Some(provider);
            state
        });
        let row_filter = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("过滤行（任意单元格包含）")
        });

        let subs = vec![
            // set_value 不发 InputEvent::Change；观察实体才能响应清除按钮和程序写值。
            cx.observe(&column_filter, |_this, _, cx| cx.notify()),
            cx.observe(&row_filter, |this: &mut Self, _, cx| {
                this.on_row_filter_input_updated(cx);
            }),
            cx.observe_global::<ramag_ui::DatabaseSearchSettingsGlobal>(|this, cx| {
                this.on_database_search_settings_changed(cx);
            }),
        ];

        Self {
            result: None,
            docs_arc: None,
            error: None,
            memory_notice: None,
            running: false,
            pagination: None,
            table: None,
            table_building: false,
            table_build_seq: 0,
            table_build_cancel: None,
            column_filter,
            row_filter,
            row_search: RowSearchState::default(),
            uniform_scroll: UniformListScrollHandle::new(),
            h_scroll: ScrollHandle::new(),
            scroll_gesture: AxisScrollGesture::default(),
            column_completion_source,
            service: None,
            config: None,
            database: String::new(),
            target_collection: None,
            pending_notification: None,
            doc_dml_busy: false,
            exporting: false,
            pending_close_dialog: false,
            selected_rows: BTreeSet::new(),
            selection_revision: 0,
            visible_selection_cache: None,
            drill_stack: Vec::new(),
            sort_by: None,
            row_view_cache: None,
            row_view_building: false,
            row_view_request_seq: 0,
            row_view_cancel: None,
            row_view_error: None,
            result_memory: None,
            _subscriptions: subs,
        }
    }

    pub fn attach_result_memory(&mut self, lease: ResultMemoryLease) {
        self.result_memory = Some(lease);
    }

    pub fn set_result_active(&self, active: bool) {
        if let Some(lease) = &self.result_memory {
            lease.set_active(active);
        }
    }

    pub fn set_context(
        &mut self,
        service: Arc<MongoService>,
        config: ConnectionConfig,
        database: String,
    ) {
        self.service = Some(service);
        self.config = Some(config);
        self.database = database;
    }

    pub fn set_target_collection(&mut self, coll: Option<String>) {
        self.target_collection = coll;
    }

    /// 写操作使用当前数据库。
    pub fn set_database(&mut self, db: String) {
        self.database = db;
    }

    /// 切库后旧结果不可写入新库。
    pub fn switch_database(&mut self, db: String, cx: &mut Context<Self>) {
        let had_result = self.running || self.result.is_some() || self.error.is_some();
        self.database = db;
        self.target_collection = None;
        self.clear_selected_rows();
        if had_result {
            self.set_error("已切换数据库，旧结果已失效；请重新运行命令".into(), cx);
        } else {
            cx.notify();
        }
    }

    /// 切换层级时清空列过滤；内容搜索保留。
    pub fn clear_column_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.column_filter
            .update(cx, |s, cx| s.set_value("", window, cx));
    }

    pub(crate) fn is_production(&self) -> bool {
        self.config.as_ref().is_some_and(|c| c.production)
    }

    pub(crate) fn can_write(&self) -> bool {
        self.service.is_some()
            && self.config.is_some()
            && self.target_collection.is_some()
            && !self.is_production()
            && !self.doc_dml_busy
    }

    pub(super) fn dml_context_matches(
        &self,
        config: &ConnectionConfig,
        database: &str,
        collection: &str,
    ) -> bool {
        self.config.as_ref().map(|current| &current.id) == Some(&config.id)
            && self.database == database
            && self.target_collection.as_deref() == Some(collection)
    }

    pub(crate) fn toggle_row(&mut self, idx: usize, cx: &mut Context<Self>) {
        if !self.selected_rows.insert(idx) {
            self.selected_rows.remove(&idx);
        }
        self.mark_selection_changed();
        cx.notify();
    }

    pub(crate) fn toggle_all(&mut self, all: &[usize], cx: &mut Context<Self>) {
        if !all.is_empty() && all.iter().all(|i| self.selected_rows.contains(i)) {
            for i in all {
                self.selected_rows.remove(i);
            }
        } else {
            self.selected_rows.extend(all.iter().copied());
        }
        self.mark_selection_changed();
        cx.notify();
    }

    pub(crate) fn clear_selected_rows(&mut self) {
        self.selected_rows.clear();
        self.mark_selection_changed();
    }

    pub(crate) fn all_visible_rows_selected(&mut self, visible: &Arc<Vec<usize>>) -> bool {
        if let Some(cache) = &self.visible_selection_cache
            && cache.selection_revision == self.selection_revision
            && Arc::ptr_eq(&cache.rows, visible)
        {
            return !visible.is_empty() && cache.visible_selected == visible.len();
        }
        let visible_selected = visible_selection_count(&self.selected_rows, visible);
        self.visible_selection_cache = Some(VisibleSelectionCache {
            rows: visible.clone(),
            selection_revision: self.selection_revision,
            visible_selected,
        });
        !visible.is_empty() && visible_selected == visible.len()
    }

    fn mark_selection_changed(&mut self) {
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.visible_selection_cache = None;
    }

    pub(crate) fn is_row_selected(&self, idx: usize) -> bool {
        self.selected_rows.contains(&idx)
    }

    pub(crate) fn toggle_sort(&mut self, path: String, cx: &mut Context<Self>) {
        self.sort_by = match self.sort_by.take() {
            Some((p, SortDir::Asc)) if p == path => Some((path, SortDir::Desc)),
            Some((p, SortDir::Desc)) if p == path => None,
            _ => Some((path, SortDir::Asc)),
        };
        self.schedule_row_view(false, cx);
    }

    pub fn set_running(&mut self, cx: &mut Context<Self>) {
        self.running = true;
        self.error = None;
        cx.notify();
    }

    /// 只读拦截后恢复原结果，避免停留在执行态。
    pub fn restore_idle(&mut self, prev_error: Option<String>, cx: &mut Context<Self>) {
        self.running = false;
        self.error = prev_error;
        cx.notify();
    }

    pub fn set_result(&mut self, mut r: MongoQueryResult, cx: &mut Context<Self>) {
        let outcome = self.account_result_bytes(r.retained_bytes, cx);
        if outcome.current_evicted {
            self.release_result_payload();
            self.error = Some(
                "结果超过全部标签 512 MiB 硬上限，已释放结果数据；查询文本仍保留，可收窄后重新运行"
                    .into(),
            );
            self.running = false;
            cx.notify();
            return;
        }
        self.memory_notice = memory_notice(&r, r.retained_bytes, outcome);
        self.clear_selected_rows();
        let label = self
            .target_collection
            .clone()
            .unwrap_or_else(|| "结果".to_string());
        let documents = Arc::new(std::mem::take(&mut r.documents));
        self.reset_drill(label, documents.clone());
        self.sort_by = None;
        self.docs_arc = Some(documents);
        self.result = Some(r);
        self.error = None;
        self.running = false;
        self.pagination = None;
        self.h_scroll.set_offset(Point::new(px(0.0), px(0.0)));
        self.scroll_gesture.reset();
        // 建基础表 + 刷新补全源（最多 200 万单元格，必须离开 UI 线程）。
        self.schedule_table_rebuild(cx);
        cx.notify();
    }

    pub fn set_error(&mut self, err: String, cx: &mut Context<Self>) {
        self.release_result_payload();
        let _ = self.account_result_bytes(0, cx);
        self.error = Some(err);
        self.running = false;
        cx.notify();
    }

    fn account_result_bytes(&self, bytes: usize, cx: &mut Context<Self>) -> ResultMemoryUpdate {
        self.result_memory
            .as_ref()
            .map_or_else(ResultMemoryUpdate::default, |lease| {
                lease.update_bytes(bytes, cx)
            })
    }

    fn release_result_payload(&mut self) {
        self.cancel_table_build();
        self.table_build_seq = self.table_build_seq.wrapping_add(1);
        self.table_building = false;
        self.invalidate_row_view();
        self.result = None;
        self.docs_arc = None;
        self.table = None;
        self.drill_stack.clear();
        self.pagination = None;
        self.memory_notice = None;
        self.column_completion_source.write().clear();
        self.clear_selected_rows();
    }

    /// 释放旧结果，保留编辑器命令。
    pub fn evict_result_for_budget(&mut self, cx: &mut Context<Self>) {
        self.release_result_payload();
        self.error =
            Some("旧结果已按 LRU 释放，以保持全部标签结果不超过 512 MiB；查询文本仍保留".into());
        self.running = false;
        cx.notify();
    }

    pub fn set_pagination(
        &mut self,
        pagination: Option<MongoResultPagination>,
        cx: &mut Context<Self>,
    ) {
        if self.pagination == pagination {
            return;
        }
        self.pagination = pagination;
        cx.notify();
    }

    pub(crate) fn open_cell_dialog(
        &self,
        column_path: String,
        kind: &'static str,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const MAX_DIALOG_PRETTY_BYTES: usize = 1024 * 1024;
        let display = pretty_cell_value(text, MAX_DIALOG_PRETTY_BYTES);
        let display = bounded_cell_dialog_text(display, MAX_DIALOG_PRETTY_BYTES);
        let title: SharedString = SharedString::from(format!(
            "{}  ({kind})",
            inline_text_preview(&column_path, 96)
        ));
        let display: SharedString = display.into();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let title = title.clone();
            let display = display.clone();
            dialog
                .title(ramag_ui::closable_dialog_title(
                    "mongo-value-detail-close",
                    title,
                    |_, _| {},
                ))
                .close_button(false)
                .w(px(720.0))
                .p(px(20.0))
                .content(move |content, _, _| {
                    content.child(
                        div()
                            .id("mongo-value-detail-scroll")
                            .w_full()
                            .h(px(400.0))
                            .overflow_y_scroll()
                            .child(
                                ramag_ui::SelectableText::new(
                                    "mongo-value-detail",
                                    display.clone(),
                                )
                                .w_full()
                                .text_sm(),
                            ),
                    )
                })
        });
    }
}

impl Drop for ResultPanel {
    fn drop(&mut self) {
        self.cancel_table_build();
        self.cancel_row_view_build();
    }
}

#[cfg(test)]
mod tests;
