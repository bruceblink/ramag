//! MongoDB 查询结果面板。

mod cell;
mod drill;
mod edit;
mod export;
mod filter;
mod flatten;
mod ops;
mod row;
mod table;
mod toolbar;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement, Point, Render,
    ScrollHandle, SharedString, Styled, UniformListScrollHandle, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use parking_lot::RwLock;
use ramag_app::MongoService;
use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, MongoQueryResult, json_pretty_bounded,
};
use ramag_ui::{ResultMemoryLease, ResultMemoryUpdate};
use serde_json::Value;

pub use flatten::FlatTable;

use crate::views::inline_text_preview;
use filter::{ParsedFilter, classify_filter, column_indices_for, row_indices_for_cancellable};

/// 过滤列补全收集的最大嵌套深度（支持 consume.detail.x 这类多层）
const PATH_COMPLETION_DEPTH: usize = 5;
/// 行过滤输入停顿后再扫描，避免每次按键都排一个最多 200 万单元格的任务。
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
    /// 表格构建代际；新结果/下钻会使旧任务回包失效。
    table_build_seq: u64,
    table_build_cancel: Option<Arc<AtomicBool>>,
    pub(crate) column_filter: Entity<InputState>,
    pub(crate) row_filter: Entity<InputState>,
    pub(crate) uniform_scroll: UniformListScrollHandle,
    pub(crate) h_scroll: ScrollHandle,
    pub(crate) column_completion_source: Arc<RwLock<Vec<String>>>,
    pub(crate) service: Option<Arc<MongoService>>,
    pub(crate) config: Option<ConnectionConfig>,
    pub(crate) database: String,
    pub(crate) target_collection: Option<String>,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pub(crate) pending_notification: Option<gpui_component::notification::Notification>,
    /// 提交中防重入，失败时保留弹框输入。
    pub(super) doc_dml_busy: bool,
    pub(super) exporting: bool,
    /// 仅成功后关闭 DML 弹框。
    pub(super) pending_close_dialog: bool,
    pub(crate) selected_rows: BTreeSet<usize>,
    /// 选择变化代次与当前可见行交集缓存，避免普通重渲染反复扫描最多五万行。
    selection_revision: u64,
    visible_selection_cache: Option<VisibleSelectionCache>,
    /// 下钻栈：栈底=原始查询结果，双击嵌套 push 一层；栈深 > 1 即下钻态（只读 + 面包屑）
    pub(crate) drill_stack: Vec<drill::DrillLevel>,
    /// 路径在钻取换表后可检测失配。
    pub(crate) sort_by: Option<(String, SortDir)>,
    /// 行过滤 + 排序派生结果；选择/弹框等重渲染时复用，避免反复扫描整张矩阵。
    row_view_cache: Option<RowViewCache>,
    /// 行过滤 / 排序正在受限工作池计算；依赖当前视图的操作在此期间禁用。
    pub(crate) row_view_building: bool,
    /// 行视图请求代次；输入、排序或表格变化后递增，旧回包不得覆盖新条件。
    row_view_request_seq: u64,
    row_view_cancel: Option<Arc<AtomicBool>>,
    /// 后台行视图构建失败时显式展示，修改条件或重建表格会清除。
    pub(crate) row_view_error: Option<String>,
    /// 当前标签的结果内存登记。
    result_memory: Option<ResultMemoryLease>,
    _subscriptions: Vec<gpui::Subscription>,
}

#[derive(Clone, Debug)]
pub enum ResultEvent {
    Refresh,
    /// 仅停止客户端等待；MongoDB 服务端操作可能仍在执行。
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
    query: String,
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
    let mut indices = row_indices_for_cancellable(table, &key.query, Some(cancelled))
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
                .placeholder("过滤列（列名逗号分隔；填 object/array 字段或 a.b 路径则钻取）");
            state.lsp.completion_provider = Some(provider);
            state
        });
        let row_filter = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("过滤行（任意单元格包含）")
        });

        let subs = vec![
            cx.subscribe(&column_filter, |_this, _, _e: &InputEvent, cx| {
                cx.notify();
            }),
            cx.subscribe(&row_filter, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.schedule_row_view(true, cx);
                }
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
            uniform_scroll: UniformListScrollHandle::new(),
            h_scroll: ScrollHandle::new(),
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

    /// 写操作必须随当前数据库切换。
    pub fn set_database(&mut self, db: String) {
        self.database = db;
    }

    /// 切库后旧结果不能作为新库的写入上下文。
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

    pub fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.column_filter
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.row_filter
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

    pub(crate) fn parse_column_filter(&self, cx: &gpui::App) -> ParsedFilter {
        let raw = self.column_filter.read(cx).value().to_string();
        let docs = self
            .drill_stack
            .last()
            .map(|level| level.documents.as_slice())
            .unwrap_or(&[]);
        classify_filter(&raw, docs)
    }

    pub(crate) fn schedule_table_rebuild(&mut self, cx: &mut Context<Self>) {
        let level = self.drill_stack.last();
        let docs = level
            .map(|l| l.documents.clone())
            .unwrap_or_else(|| Arc::new(Vec::new()));
        let ancestors = level.map(|l| l.ancestors.clone()).unwrap_or_default();
        self.cancel_table_build();
        self.table_build_seq = self.table_build_seq.wrapping_add(1);
        let request_seq = self.table_build_seq;
        self.table = None;
        let raw_bytes = self
            .result
            .as_ref()
            .map_or(0, |result| result.retained_bytes);
        let _ = self.account_result_bytes(raw_bytes, cx);
        self.invalidate_row_view();
        self.table_building = !docs.is_empty();
        self.column_completion_source.write().clear();
        if docs.is_empty() {
            return;
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        self.table_build_cancel = Some(cancelled.clone());

        cx.spawn(async move |this, cx| {
            let worker_cancelled = cancelled.clone();
            let built = ramag_app::run_blocking(move || {
                let Some(mut ft) = flatten::build_flat_table_with_cancellable(
                    docs.as_slice(),
                    &BTreeSet::new(),
                    &worker_cancelled,
                ) else {
                    return Ok(None);
                };
                // 下钻层保留从根到父级的祖先列。
                if !ancestors.is_empty() {
                    let lead = ancestors
                        .into_iter()
                        .map(|(label, cell)| {
                            let kind = if cell.kind == "null" {
                                "text"
                            } else {
                                cell.kind
                            };
                            let path = drill::ancestor_id_column_name(&label);
                            (flatten::Column { path, kind }, cell)
                        })
                        .collect();
                    if !ft.prepend_constant_lead_cancellable(lead, &worker_cancelled) {
                        return Ok(None);
                    }
                }
                let Some(completions) = flatten::collect_paths_cancellable(
                    docs.as_slice(),
                    PATH_COMPLETION_DEPTH,
                    &worker_cancelled,
                ) else {
                    return Ok(None);
                };
                Ok(Some((Arc::new(ft), completions)))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.table_build_seq != request_seq
                    || !this
                        .table_build_cancel
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
                {
                    return;
                }
                this.table_build_cancel = None;
                this.table_building = false;
                match built {
                    Ok(Some((table, completions))) => {
                        let combined_bytes = this
                            .result
                            .as_ref()
                            .map_or(0, |result| result.retained_bytes)
                            .saturating_add(table.retained_bytes());
                        let outcome = this.account_result_bytes(combined_bytes, cx);
                        if outcome.current_evicted {
                            this.release_result_payload();
                            this.error = Some(
                                "MongoDB 结果及表格视图超过全部标签 512 MiB 硬上限，已释放；请收窄查询"
                                    .into(),
                            );
                            cx.notify();
                            return;
                        }
                        if let Some(result) = &this.result
                            && let Some(notice) = memory_notice(result, combined_bytes, outcome)
                        {
                            // 表格占用更新不清除刚产生的 LRU 提示。
                            this.memory_notice = Some(notice);
                        }
                        this.table = Some(table);
                        *this.column_completion_source.write() = completions;
                        this.schedule_row_view(false, cx);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.invalidate_row_view();
                        this.error = Some(format!("构建 MongoDB 结果表格失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn filtered_column_indices(&self, cx: &gpui::App) -> Option<Vec<usize>> {
        column_indices_for(self.table.as_ref()?, &self.parse_column_filter(cx).filters)
    }

    pub(crate) fn display_row_indices(&self, cx: &gpui::App) -> Option<(Arc<Vec<usize>>, bool)> {
        let query = self.row_filter.read(cx).value().trim().to_string();
        let filtered = !query.is_empty();
        let key = RowViewKey {
            generation: self.table_build_seq,
            query,
            sort_by: self.sort_by.clone(),
        };
        self.row_view_cache
            .as_ref()
            .filter(|cache| cache.key == key)
            .map(|cache| (cache.indices.clone(), filtered))
    }

    fn invalidate_row_view(&mut self) {
        self.cancel_row_view_build();
        self.row_view_request_seq = self.row_view_request_seq.wrapping_add(1);
        self.row_view_cache = None;
        self.row_view_building = false;
        self.row_view_error = None;
    }

    /// 去抖后在受限工作池扫描，旧条件回包按代际丢弃。
    fn schedule_row_view(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let Some(table) = self.table.clone() else {
            self.invalidate_row_view();
            cx.notify();
            return;
        };
        let key = RowViewKey {
            generation: self.table_build_seq,
            query: self.row_filter.read(cx).value().trim().to_string(),
            sort_by: self.sort_by.clone(),
        };
        if self
            .row_view_cache
            .as_ref()
            .is_some_and(|cache| cache.key == key)
        {
            self.row_view_building = false;
            self.row_view_error = None;
            cx.notify();
            return;
        }

        self.cancel_row_view_build();
        self.row_view_request_seq = self.row_view_request_seq.wrapping_add(1);
        let request_seq = self.row_view_request_seq;
        self.row_view_cache = None;
        self.row_view_building = true;
        self.row_view_error = None;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.row_view_cancel = Some(cancelled.clone());
        cx.notify();

        cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(ROW_VIEW_DEBOUNCE).await;
            }
            let current = this
                .update(cx, |this, _| {
                    this.row_view_request_seq == request_seq
                        && this.table_build_seq == key.generation
                        && this
                            .row_view_cancel
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
                })
                .unwrap_or(false);
            if !current {
                return;
            }

            let worker_key = key.clone();
            let worker_cancelled = cancelled.clone();
            let built = ramag_app::run_blocking(move || {
                Ok(build_row_view_indices(
                    &table,
                    &worker_key,
                    &worker_cancelled,
                ))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.row_view_request_seq != request_seq
                    || this.table_build_seq != key.generation
                    || !this
                        .row_view_cancel
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
                {
                    return;
                }
                this.row_view_cancel = None;
                this.row_view_building = false;
                match built {
                    Ok(Some(indices)) => {
                        this.row_view_cache = Some(RowViewCache { key, indices });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.row_view_error = Some(format!("构建行视图失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_table_build(&mut self) {
        if let Some(cancelled) = self.table_build_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn cancel_row_view_build(&mut self) {
        if let Some(cancelled) = self.row_view_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
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
        let display = if text.len() <= MAX_DIALOG_PRETTY_BYTES
            && (text.starts_with('{') || text.starts_with('['))
        {
            serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| json_pretty_bounded(&value, MAX_DIALOG_PRETTY_BYTES))
                .unwrap_or(text)
        } else {
            text
        };
        let display = bounded_cell_dialog_text(display, MAX_DIALOG_PRETTY_BYTES);
        let title: SharedString = SharedString::from(format!(
            "{}  ({kind})",
            inline_text_preview(&column_path, 96)
        ));
        let input: Entity<InputState> = cx.new(|cx_inner| {
            InputState::new(window, cx_inner)
                .multi_line(true)
                .default_value(display)
        });
        window.open_dialog(cx, move |dialog, _w, _app| {
            let input = input.clone();
            let title = title.clone();
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
                            .w_full()
                            .h(px(400.0))
                            .child(Input::new(&input).small().h_full().disabled(true)),
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

fn bounded_cell_dialog_text(mut text: String, max_bytes: usize) -> String {
    const TRUNCATED_NOTICE: &str = "\n\n[内容过大，仅显示开头部分]";
    if text.len() <= max_bytes {
        return text;
    }

    let mut end = max_bytes.saturating_sub(TRUNCATED_NOTICE.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(TRUNCATED_NOTICE);
    text
}

fn memory_notice(
    result: &MongoQueryResult,
    retained_bytes: usize,
    outcome: ResultMemoryUpdate,
) -> Option<String> {
    let mut notices = Vec::new();
    if result.memory_warning
        || retained_bytes >= ramag_domain::entities::INTERACTIVE_RESULT_WARNING_BYTES
    {
        notices.push(
            "单个结果及表格视图已达到 128 MiB 提示线，建议用 filter / projection 收窄查询"
                .to_string(),
        );
    }
    if outcome.warning {
        if outcome.evicted_results > 0 {
            notices.push(format!(
                "全部查询标签结果达到全局预算，已按 LRU 释放 {} 个非活动标签的旧结果",
                outcome.evicted_results
            ));
        } else {
            notices.push(format!(
                "全部查询标签结果已达到 384 MiB 提示线（当前约 {} MiB）",
                outcome.total_bytes / 1024 / 1024
            ));
        }
    }
    (!notices.is_empty()).then(|| notices.join("；"))
}

impl Render for ResultPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }
        if std::mem::take(&mut self.pending_close_dialog) {
            window.close_dialog(cx);
        }
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        if self.running {
            return v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(empty_hint("执行中…", muted))
                .into_any_element();
        }
        if let Some(err) = self.error.clone() {
            return v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(error_hint(err, danger))
                .into_any_element();
        }
        let Some((affected, elapsed, truncated)) = self
            .result
            .as_ref()
            .map(|r| (r.affected, r.elapsed_ms, r.truncated))
        else {
            return v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(empty_hint(
                    "（点击左侧 collection 自动开 Tab，或在编辑器写命令后运行）",
                    muted,
                ))
                .into_any_element();
        };
        let total_docs = self.docs_arc.as_ref().map_or(0, |docs| docs.len());
        let Some(table_arc) = self.table.clone() else {
            let hint = if self.table_building {
                format!("正在构建表格视图…（{total_docs} 条文档）")
            } else if affected > 0 {
                format!("已执行写操作，影响 {affected} 条")
            } else if self.is_drilled() {
                "（空）".to_string()
            } else {
                "（无文档返回）".to_string()
            };
            let mut root = v_flex().size_full().bg(bg).child(toolbar::render(self, cx));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root.child(empty_hint(hint, muted)).into_any_element();
        };

        // 路径过滤可临时生成只读钻取视图。
        if let Some((flat_docs, flat_table, drill_path)) = self.try_drill_path(cx) {
            let n = flat_docs.len();
            let filters = self.parse_column_filter(cx).filters;
            let col_indices = column_indices_for(&flat_table, &filters);
            let row_q = self.row_filter.read(cx).value().trim().to_string();
            let mut row_indices = row_indices_for_cancellable(&flat_table, &row_q, None)
                .ok()
                .flatten()
                .unwrap_or_else(|| (0..flat_table.rows.len()).collect());
            if let Some((sort_path, dir)) = self.sort_by.clone()
                && let Some(ci) = flat_table.columns.iter().position(|c| c.path == sort_path)
            {
                let numeric = matches!(
                    flat_table.columns[ci].kind,
                    "int" | "long" | "double" | "decimal"
                );
                table::sort_row_indices(&flat_table, ci, numeric, dir, &mut row_indices);
            }
            let row_indices = Arc::new(row_indices);
            let mut root = v_flex()
                .size_full()
                .bg(bg)
                .child(toolbar::render(self, cx))
                .child(div().h(px(1.0)).bg(border))
                .child(flatten_hint(&drill_path, n, border, muted, bg));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root
                .child(div().flex_1().min_h_0().child(table::render(
                    self,
                    flat_table,
                    col_indices,
                    row_indices,
                    false,
                    Some(flat_docs),
                    cx,
                )))
                .child(render_status_bar(
                    format!("钻取「{drill_path}」· {n} 条"),
                    border,
                    muted,
                    bg,
                    self.pagination,
                    cx.entity(),
                ))
                .into_any_element();
        }

        let col_indices = self.filtered_column_indices(cx);
        let Some((row_indices, rows_filtered)) = self.display_row_indices(cx) else {
            let hint = if self.row_view_building {
                format!("正在筛选 / 排序…（{total_docs} 行）")
            } else if let Some(error) = &self.row_view_error {
                error.clone()
            } else {
                "正在准备行视图…".to_string()
            };
            let mut root = v_flex().size_full().bg(bg).child(toolbar::render(self, cx));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root.child(empty_hint(hint, muted)).into_any_element();
        };
        let filtered_rows = row_indices.len();
        let visible_selected = if rows_filtered {
            row_indices
                .iter()
                .filter(|ri| self.selected_rows.contains(ri))
                .count()
        } else {
            self.selected_rows.len()
        };
        let hidden_selected = self.selected_rows.len().saturating_sub(visible_selected);
        let total_cols = self.table.as_ref().map(|t| t.columns.len()).unwrap_or(0);
        let discovered_cols = table_arc.total_columns;
        let visible_cols_count = col_indices.as_ref().map(|v| v.len()).unwrap_or(total_cols);
        let mut summary = match (rows_filtered, col_indices.is_some()) {
            (true, true) => format!(
                "命中 {visible_cols_count} / {total_cols} 列 · {filtered_rows} / {total_docs} 行 · 耗时 {elapsed}ms"
            ),
            (true, false) => format!("命中 {filtered_rows} / {total_docs} 行 · 耗时 {elapsed}ms"),
            (false, true) => format!(
                "命中 {visible_cols_count} / {total_cols} 列 · {total_docs} 行 · 耗时 {elapsed}ms"
            ),
            (false, false) => format!("{total_docs} 行 · 耗时 {elapsed}ms"),
        };
        if !self.selected_rows.is_empty() {
            if hidden_selected > 0 {
                summary.push_str(&format!(
                    " · 已选 {} 行，其中 {hidden_selected} 行当前隐藏",
                    self.selected_rows.len()
                ));
            } else {
                summary.push_str(&format!(" · 已选 {} 行", self.selected_rows.len()));
            }
        }

        let mut root = v_flex()
            .size_full()
            .bg(bg)
            .child(toolbar::render(self, cx))
            .child(div().h(px(1.0)).bg(border));
        if let Some(notice) = self.memory_notice.clone() {
            let warn = cx.theme().warning;
            let mut warn_bg = warn;
            warn_bg.a = 0.14;
            root = root.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(5.0))
                    .bg(warn_bg)
                    .text_xs()
                    .text_color(warn)
                    .child(format!("⚠ {notice}")),
            );
        }
        if truncated {
            let warn = cx.theme().warning;
            let mut warn_bg = warn;
            warn_bg.a = 0.14;
            let message = if self.pagination.is_some() {
                format!(
                    "⚠ 当前页达到 256 MiB 硬上限，仅加载 {total_docs} 条；可从实际断点继续翻页，统计、排序、过滤与导出均只基于当前页。"
                )
            } else {
                format!(
                    "⚠ 结果较大，仅加载前 {total_docs} 条；统计、排序、过滤与导出均基于这部分数据。请用 filter / limit 精确查询"
                )
            };
            root = root.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(5.0))
                    .bg(warn_bg)
                    .text_xs()
                    .text_color(warn)
                    .child(message),
            );
        }
        if discovered_cols > total_cols {
            let warn = cx.theme().warning;
            let mut warn_bg = warn;
            warn_bg.a = 0.14;
            root = root.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(5.0))
                    .bg(warn_bg)
                    .text_xs()
                    .text_color(warn)
                    .child(format!(
                        "⚠ 字段较多，表格仅展示前 {total_cols} 列；完整文档详情仍保留，表格筛选与 CSV 导出基于已展示列。"
                    )),
            );
        }
        if self.is_drilled() {
            root = root.child(self.render_breadcrumb(cx));
        }
        root.child(div().flex_1().min_h_0().child(table::render(
            self,
            table_arc,
            col_indices,
            row_indices,
            true,
            None,
            cx,
        )))
        .child(render_status_bar(
            summary,
            border,
            muted,
            bg,
            self.pagination,
            cx.entity(),
        ))
        .into_any_element()
    }
}

fn render_status_bar(
    summary: String,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    bg: gpui::Hsla,
    pagination: Option<MongoResultPagination>,
    panel: Entity<ResultPanel>,
) -> impl IntoElement {
    h_flex()
        .id("mongo-status-bar")
        .w_full()
        .flex_none()
        .items_center()
        .gap_2()
        .px(px(12.0))
        .py(px(4.0))
        .border_t_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .text_color(muted)
        .child(SharedString::from(summary))
        .child(div().flex_1())
        .when_some(pagination, |this, pagination| {
            let previous_page = pagination.page.saturating_sub(1);
            let next_page = pagination.page.saturating_add(1);
            let panel_for_previous = panel.clone();
            let panel_for_next = panel.clone();
            this.child(
                ramag_ui::clickable_button("mongo-result-page-previous")
                    .ghost()
                    .small()
                    .label("上页")
                    .disabled(pagination.page == 0)
                    .on_click(move |_, _, app| {
                        panel_for_previous.update(app, |_, cx| {
                            cx.emit(ResultEvent::PageRequested(previous_page));
                        });
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .child(format!("第 {} 页", pagination.page + 1)),
            )
            .child(
                ramag_ui::clickable_button("mongo-result-page-next")
                    .ghost()
                    .small()
                    .label("下页")
                    .tooltip("未指定 sort 时分页顺序不固定")
                    .disabled(!pagination.has_more)
                    .on_click(move |_, _, app| {
                        panel_for_next.update(app, |_, cx| {
                            cx.emit(ResultEvent::PageRequested(next_page));
                        });
                    }),
            )
        })
}

fn flatten_hint(
    path: &str,
    n: usize,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    bg: gpui::Hsla,
) -> impl IntoElement {
    div()
        .id("mongo-flatten-hint")
        .w_full()
        .flex_none()
        .px(px(12.0))
        .py(px(5.0))
        .border_b_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .text_color(muted)
        .child(SharedString::from(format!(
            "已钻取「{path}」· {n} 条（清空上方过滤列恢复）"
        )))
}

fn empty_hint(text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    div()
        .id("mongo-result-hint")
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(12.0))
        .py(px(10.0))
        .text_xs()
        .text_color(color)
        .child(text.into())
}

fn error_hint(text: String, color: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    div()
        .id("mongo-result-error")
        .flex_1()
        .px(px(12.0))
        .py(px(10.0))
        .text_xs()
        .text_color(color)
        .child(SharedString::from(text))
}

#[cfg(test)]
mod row_view_tests {
    use super::cell::Cell;
    use super::flatten::Column;
    use super::*;

    fn table() -> FlatTable {
        FlatTable {
            columns: vec![
                Column {
                    path: "name".into(),
                    kind: "text",
                },
                Column {
                    path: "n".into(),
                    kind: "int",
                },
            ],
            total_columns: 2,
            rows: [("Bob", "10"), ("Alice", "2"), ("Bobby", "1")]
                .into_iter()
                .map(|(name, number)| {
                    vec![
                        Cell {
                            text: name.into(),
                            kind: "text",
                        },
                        Cell {
                            text: number.into(),
                            kind: "int",
                        },
                    ]
                })
                .collect(),
        }
    }

    #[test]
    fn row_view_combines_filter_and_numeric_sort() {
        let key = RowViewKey {
            generation: 1,
            query: "bo".into(),
            sort_by: Some(("n".into(), SortDir::Desc)),
        };

        let cancelled = AtomicBool::new(false);
        let indices = build_row_view_indices(&table(), &key, &cancelled).unwrap();

        assert_eq!(indices.as_slice(), &[0, 2]);
    }

    #[test]
    fn row_view_stops_before_scanning_when_cancelled() {
        let key = RowViewKey {
            generation: 1,
            query: "bo".into(),
            sort_by: None,
        };
        let cancelled = AtomicBool::new(true);

        assert!(build_row_view_indices(&table(), &key, &cancelled).is_none());
    }

    #[test]
    fn row_view_cache_key_changes_with_generation() {
        let key = RowViewKey {
            generation: 2,
            query: String::new(),
            sort_by: None,
        };
        let cache = RowViewCache {
            key: key.clone(),
            indices: Arc::new(vec![0, 1]),
        };
        assert_eq!(cache.key, key);

        let mut stale = key;
        stale.generation += 1;
        assert_ne!(cache.key, stale);
    }

    #[test]
    fn visible_selection_count_ignores_hidden_rows() {
        let selected = BTreeSet::from([0, 2, 4]);

        assert_eq!(visible_selection_count(&selected, &[2, 3, 4]), 2);
    }

    #[test]
    fn cell_dialog_text_is_unicode_safe_and_bounded() {
        let display = bounded_cell_dialog_text("你".repeat(40), 64);

        assert!(display.len() <= 64);
        assert!(display.is_char_boundary(display.len()));
        assert!(display.contains("[内容过大"));
    }
}
