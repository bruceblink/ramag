mod export;
mod helpers;
mod ops;
mod render;
mod scroll;

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use parking_lot::RwLock;

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, Point, ScrollHandle, ScrollStrategy,
    UniformListScrollHandle, Window, px,
};
use gpui_component::input::InputState;
use gpui_component::notification::Notification;
use ramag_app::ConnectionService;
use ramag_domain::entities::{
    Column, ConnectionConfig, MAX_SQL_QUERY_BYTES, QueryResult, Value, Warning,
};
use ramag_ui::{AxisScrollGesture, ResultMemoryLease, ResultMemoryUpdate};

use crate::sql_completion::SchemaCache;
use helpers::{PendingInsert, extract_first_table_ref, parse_value_for_kind};

pub(crate) use helpers::{RowIdentity, derive_row_identity};

/// 服务端分页的可见页大小，也是未分页结果的 UI 渲染上限。
pub(super) const MAX_ROWS_DISPLAY: usize = 10_000;
/// 行内新增最多创建的输入框数量，避免异常元数据一次生成数万控件。
pub(super) const MAX_INSERT_COLUMNS: usize = 512;

#[derive(Debug, Clone, Default)]
pub enum ResultState {
    #[default]
    Empty,
    Running,
    Error(String),
    /// 因全局预算释放的结果。
    Released(String),
    Ok(Arc<QueryResult>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// 全表精确总行数的异步计数状态。分页刻意只用哨兵判断有无下一页，
/// 精确总数由首屏后台 COUNT(*) 回填，故需三态区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TotalRows {
    Counting,
    Known(u64),
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResultPagination {
    pub(crate) page: usize,
    pub(crate) page_size: usize,
    pub(crate) has_more: bool,
    /// 全表精确总行数状态；跨翻页缓存复用，不逐页重算。
    pub(crate) total: TotalRows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultPanelEvent {
    PageRequested(usize),
}

impl EventEmitter<ResultPanelEvent> for ResultPanel {}

struct VisibleSelectionCache {
    rows: Arc<Vec<usize>>,
    selection_revision: u64,
    visible_selected: usize,
}

pub struct ResultPanel {
    pub(super) state: ResultState,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pub(super) pending_notification: Option<Notification>,
    pub(super) selected_cell: Option<(usize, usize)>,
    pub(super) selected_rows: BTreeSet<usize>,
    /// 选择变化代次与当前可见行交集缓存，避免普通重渲染反复扫描最多一万行。
    selection_revision: u64,
    visible_selection_cache: Option<VisibleSelectionCache>,
    pub(super) source_sql: Option<String>,
    pub(super) pinned_target: Option<(Option<String>, String)>,
    /// 行定位键（真实主键 / 全非空唯一索引）：QueryTab 查询成功后异步注入；
    /// None = 元数据未就绪或该表无键，行内修改 / 删除一律禁用
    pub(super) row_identity: Option<RowIdentity>,
    pub(super) col_width_overrides: Vec<Option<gpui::Pixels>>,
    /// DML（增删改）防重入闸：spawn 前置位、回包复位；置位期间再次提交被 dml_conn 拦下
    pub(super) dml_busy: bool,
    /// 导出防重入闸；后台任务完成（含取消/失败）后复位。
    pub(super) exporting: bool,
    pub(super) sort_by: Option<(usize, SortDir)>,
    /// 当前服务端结果页；None 表示本次 SQL 不具备安全分页资格。
    pub(super) pagination: Option<ResultPagination>,
    /// 结果内容代次：状态切换或本地增删改后递增，用于派生视图缓存和异步回包校验。
    pub(super) result_revision: u64,
    /// 排序、筛选及列布局的派生缓存；选择单元格等无关重渲染可直接复用。
    pub(super) display_view_cache: Option<crate::views::result_table::DisplayViewCache>,
    /// 当前后台派生视图的条件；用于避免普通重渲染重复排队同一任务。
    pub(super) display_view_build_key: Option<crate::views::result_table::DisplayViewCacheKey>,
    /// 派生视图构建状态与精确取消令牌。
    pub(super) display_view_building: bool,
    pub(super) display_view_cancel: Option<Arc<AtomicBool>>,
    pub(super) display_view_request_seq: u64,
    pub(super) display_view_error: Option<String>,
    pub(super) column_filter_input: Entity<InputState>,
    pub(super) row_filter_input: Entity<InputState>,
    pub(super) cell_edit_input: Option<Entity<InputState>>,
    pub(super) service: Option<Arc<ConnectionService>>,
    pub(super) connection: Option<ConnectionConfig>,
    pub(super) schema_cache: Option<Arc<RwLock<SchemaCache>>>,
    pub(super) pending_insert: Option<PendingInsert>,
    pub(super) uniform_scroll: UniformListScrollHandle,
    pub(super) h_scroll: ScrollHandle,
    /// 结果表触控板手势的轴锁定状态，必须跨帧保留。
    result_scroll_gesture: AxisScrollGesture,
    pub(super) column_completion_source: Arc<RwLock<Vec<String>>>,
    pub(super) warnings_expanded: bool,
    /// 当前标签的结果内存登记。
    result_memory: Option<ResultMemoryLease>,
}

impl ResultPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let column_completion_source: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
        let provider = crate::sql_completion::ColumnFilterCompletionProvider::new_rc(
            column_completion_source.clone(),
        );
        let column_filter_input = cx.new(|cx| {
            let mut state =
                ramag_ui::bounded_search_input(window, cx).placeholder("过滤列（逗号分隔多列名）");
            state.lsp.completion_provider = Some(provider);
            state
        });
        let row_filter_input = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx).placeholder("过滤行（任意单元格包含）")
        });
        cx.observe(&column_filter_input, |_, _, cx| cx.notify())
            .detach();
        cx.observe(&row_filter_input, |_, _, cx| cx.notify())
            .detach();

        Self {
            state: ResultState::Empty,
            pending_notification: None,
            selected_cell: None,
            source_sql: None,
            col_width_overrides: Vec::new(),
            dml_busy: false,
            exporting: false,
            sort_by: None,
            pagination: None,
            result_revision: 0,
            display_view_cache: None,
            display_view_build_key: None,
            display_view_building: false,
            display_view_cancel: None,
            display_view_request_seq: 0,
            display_view_error: None,
            column_filter_input,
            row_filter_input,
            cell_edit_input: None,
            service: None,
            connection: None,
            schema_cache: None,
            pinned_target: None,
            row_identity: None,
            selected_rows: BTreeSet::new(),
            selection_revision: 0,
            visible_selection_cache: None,
            pending_insert: None,
            uniform_scroll: UniformListScrollHandle::new(),
            h_scroll: ScrollHandle::new(),
            result_scroll_gesture: AxisScrollGesture::default(),
            column_completion_source,
            warnings_expanded: false,
            result_memory: None,
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

    pub(super) fn uniform_scroll(&self) -> &UniformListScrollHandle {
        &self.uniform_scroll
    }

    pub(super) fn h_scroll(&self) -> &ScrollHandle {
        &self.h_scroll
    }

    pub fn start_insert(
        &mut self,
        columns: Vec<Column>,
        inputs: Vec<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) {
        self.pending_insert = Some(PendingInsert { columns, inputs });
        let pending_idx = if let ResultState::Ok(qr) = &self.state {
            qr.rows.len().min(MAX_ROWS_DISPLAY)
        } else {
            0
        };
        self.uniform_scroll
            .scroll_to_item(pending_idx, ScrollStrategy::Center);
        cx.notify();
    }

    pub(crate) fn pending_insert(&self) -> Option<&PendingInsert> {
        self.pending_insert.as_ref()
    }

    pub(super) fn cancel_insert(&mut self, cx: &mut Context<Self>) {
        self.pending_insert = None;
        cx.notify();
    }

    pub(super) fn submit_insert(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_insert.as_ref() else {
            return;
        };
        let mut values: Vec<(String, Value)> = Vec::new();
        let mut err: Option<String> = None;
        let mut total_input_bytes = 0usize;
        for (col, input) in pending.columns.iter().zip(pending.inputs.iter()) {
            let input_value = input.read(cx).value();
            let Some(next_total) = total_input_bytes
                .checked_add(input_value.len())
                .filter(|total| *total <= MAX_SQL_QUERY_BYTES)
            else {
                err = Some(format!(
                    "新增行内容合计超过 {} MiB 安全上限，请改用 SQL 分批写入",
                    MAX_SQL_QUERY_BYTES / 1024 / 1024
                ));
                break;
            };
            total_input_bytes = next_total;
            let text = input_value.to_string();
            let nullable = col.nullable;
            let has_default = col.default_value.is_some() || col.is_primary_key;
            match parse_value_for_kind(col.data_type.kind, &text, nullable, has_default) {
                Ok(Some(v)) => values.push((col.name.clone(), v)),
                Ok(None) => {}
                Err(msg) => {
                    err = Some(format!("{}: {msg}", col.name));
                    break;
                }
            }
        }
        if let Some(msg) = err {
            self.pending_notification = Some(Notification::error(msg).autohide(true));
            cx.notify();
            return;
        }
        if values.is_empty() {
            self.pending_notification = Some(Notification::warning("请至少填一列").autohide(true));
            cx.notify();
            return;
        }
        if self.apply_insert_async(values, cx) {
            // 请求已成功发起后退出草稿模式；前置校验失败时保留用户输入。
            self.pending_insert = None;
            cx.notify();
        }
    }

    /// 注入精确目标表，避免 SQL 解析误差。
    pub fn set_pinned_target(&mut self, target: Option<(Option<String>, String)>) {
        self.pinned_target = target;
    }

    /// 仅向匹配的结果集注入行定位键。
    pub(crate) fn set_row_identity_if_target(
        &mut self,
        schema: &str,
        table: &str,
        identity: Option<RowIdentity>,
        cx: &mut Context<Self>,
    ) {
        let matches_target = self
            .pinned_target
            .as_ref()
            .is_some_and(|(s, t)| s.as_deref() == Some(schema) && t == table);
        if matches_target {
            self.row_identity = identity;
            cx.notify();
        }
    }

    /// 手改 SQL 后清除可编辑目标。
    pub fn clear_editable_target(&mut self, cx: &mut Context<Self>) {
        if self.pinned_target.is_some() || self.row_identity.is_some() {
            self.pinned_target = None;
            self.row_identity = None;
            cx.notify();
        }
    }

    /// 返回行内新增的禁用原因。
    pub(crate) fn insert_block_reason(&self) -> Option<&'static str> {
        if self.dml_busy {
            return Some("上一写操作尚未完成");
        }
        let Some(conn) = &self.connection else {
            return Some("未注入连接");
        };
        if conn.production {
            return Some("生产连接 · 只读");
        }
        if self.pinned_target.is_none() {
            return Some("仅表树打开的单表数据支持增删改；手写 / 手改 SQL 的结果为只读");
        }
        if self.target_is_view() {
            return Some("视图不可写入");
        }
        None
    }

    pub(super) fn dml_busy(&self) -> bool {
        self.dml_busy
    }

    /// 返回行内修改或删除的禁用原因。
    pub(crate) fn modify_block_reason(&self) -> Option<&'static str> {
        if let Some(r) = self.insert_block_reason() {
            return Some(r);
        }
        if self.row_identity.is_none() {
            return Some("该表无主键或全非空唯一索引，禁用行内修改 / 删除");
        }
        None
    }

    /// 当前结果集对应的目标表的引用字符串：优先用 pinned_target，再回退 SQL 解析
    pub(super) fn current_table_ref(&self) -> Option<String> {
        let driver = self.connection.as_ref().map(|c| c.driver)?;
        if let Some((schema, table)) = &self.pinned_target {
            return Some(match schema {
                Some(s) => format!(
                    "{}.{}",
                    driver.quote_identifier(s),
                    driver.quote_identifier(table)
                ),
                None => driver.quote_identifier(table),
            });
        }
        self.source_sql
            .as_deref()
            .and_then(|sql| extract_first_table_ref(sql, driver))
    }

    pub fn set_executor(
        &mut self,
        service: Option<Arc<ConnectionService>>,
        connection: Option<ConnectionConfig>,
    ) {
        self.service = service;
        self.connection = connection;
    }

    pub fn set_schema_cache(&mut self, cache: Option<Arc<RwLock<SchemaCache>>>) {
        self.schema_cache = cache;
    }

    pub(super) fn target_is_view(&self) -> bool {
        let Some(cache) = &self.schema_cache else {
            return false;
        };
        if let Some((schema, table)) = &self.pinned_target {
            return cache.read().is_view(schema.as_deref(), table);
        }
        let Some(sql) = self.source_sql.as_deref() else {
            return false;
        };
        let tables = crate::sql_completion::extract_tables_in_use_for_prefetch(sql);
        let Some((schema, table)) = tables.into_iter().next() else {
            return false;
        };
        cache.read().is_view(schema.as_deref(), &table)
    }

    pub(super) fn set_cell_edit_input(&mut self, input: Option<Entity<InputState>>) {
        self.cell_edit_input = input;
    }

    pub(super) fn cell_info(&self, ri: usize, ci: usize) -> Option<(String, String, bool)> {
        let ResultState::Ok(result) = &self.state else {
            return None;
        };
        let col_name = result.columns.get(ci)?.clone();
        let val = result.rows.get(ri)?.values.get(ci)?;
        let (display, truncated) = val.display_for_edit_bounded(MAX_SQL_QUERY_BYTES);
        Some((col_name, display, truncated))
    }

    /// 返回单元格编辑的只读原因。
    pub(super) fn cell_edit_block_reason(&self, ri: usize, ci: usize) -> Option<String> {
        if let Some(reason) = self.modify_block_reason() {
            return Some(reason.to_string());
        }
        if self.cell_is_binary(ri, ci) {
            return Some(
                "二进制内容显示为 hex 文本，直接保存会损坏原始字节，仅可查看 / 复制".to_string(),
            );
        }
        None
    }

    pub(super) fn identity_label(&self) -> &'static str {
        self.row_identity
            .as_ref()
            .map(|i| i.label)
            .unwrap_or("主键")
    }

    fn preview_col_idx(&self, result: &QueryResult) -> usize {
        self.row_identity
            .as_ref()
            .and_then(|ident| ident.columns.first())
            .and_then(|key| {
                result
                    .columns
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(key))
            })
            .unwrap_or(0)
    }

    /// 二进制显示值无法无损回写，必须只读。
    pub(super) fn cell_is_binary(&self, ri: usize, ci: usize) -> bool {
        let ResultState::Ok(result) = &self.state else {
            return false;
        };
        matches!(
            result.rows.get(ri).and_then(|r| r.values.get(ci)),
            Some(ramag_domain::entities::Value::Bytes(_))
        )
    }

    pub fn set_source_sql(&mut self, sql: Option<String>) {
        self.source_sql = sql;
    }

    pub fn set_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
        let state = self.account_result_memory(state, cx);
        if matches!(&state, ResultState::Released(_)) {
            self.clear_released_result_context();
        }
        let has_client_warning = matches!(
            &state,
            ResultState::Ok(qr) if qr.warnings.iter().any(|warning| warning.level == "Client")
        );
        match &state {
            ResultState::Ok(qr) => {
                *self.column_completion_source.write() = qr.columns.clone();
            }
            _ => {
                self.column_completion_source.write().clear();
            }
        }
        self.state = state;
        self.pagination = None;
        self.mark_result_changed();
        self.selected_cell = None;
        self.clear_selected_rows();
        self.sort_by = None;
        self.col_width_overrides.clear();
        self.pending_insert = None;
        // 客户端资源警告直接展开，避免用户把已截断结果误认为完整结果。
        self.warnings_expanded = has_client_warning;
        self.row_identity = None;
        self.uniform_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.h_scroll.set_offset(Point::new(px(0.0), px(0.0)));
        self.result_scroll_gesture.reset();
        cx.notify();
    }

    /// 恢复状态快照，不清理选择、排序与滚动位置。
    pub fn restore_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
        let state = self.account_result_memory(state, cx);
        if matches!(&state, ResultState::Released(_)) {
            self.clear_released_result_context();
        }
        if let ResultState::Ok(qr) = &state {
            *self.column_completion_source.write() = qr.columns.clone();
        }
        self.state = state;
        self.mark_result_changed();
        cx.notify();
    }

    fn account_result_memory(&self, mut state: ResultState, cx: &mut Context<Self>) -> ResultState {
        let bytes = match &state {
            ResultState::Ok(result) => {
                usize::try_from(result.retained_bytes()).unwrap_or(usize::MAX)
            }
            _ => 0,
        };
        let Some(lease) = &self.result_memory else {
            return state;
        };
        let outcome = lease.update_bytes(bytes, cx);
        if outcome.current_evicted {
            return ResultState::Released(
                "结果超过全部标签 512 MiB 硬上限，已释放结果数据；查询文本仍保留，可收窄后重新运行"
                    .into(),
            );
        }
        if outcome.warning
            && let ResultState::Ok(result) = &mut state
        {
            let result = Arc::make_mut(result);
            if !result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("全部查询标签结果"))
            {
                result.warnings.push(global_memory_warning(outcome));
            }
        }
        state
    }

    /// 释放旧结果，保留编辑器中的查询文本。
    pub fn evict_result_for_budget(&mut self, cx: &mut Context<Self>) {
        self.state = ResultState::Released(
            "旧结果已按 LRU 释放，以保持全部标签结果不超过 512 MiB；查询文本仍保留".into(),
        );
        self.clear_released_result_context();
        self.column_completion_source.write().clear();
        self.pagination = None;
        self.mark_result_changed();
        self.selected_cell = None;
        self.clear_selected_rows();
        self.sort_by = None;
        self.col_width_overrides.clear();
        self.pending_insert = None;
        self.row_identity = None;
        cx.notify();
    }

    fn clear_released_result_context(&mut self) {
        self.source_sql = None;
        self.pinned_target = None;
        self.cell_edit_input = None;
        self.row_identity = None;
    }

    /// 标记结果变化并丢弃派生缓存。
    pub(super) fn mark_result_changed(&mut self) {
        self.result_revision = self.result_revision.wrapping_add(1);
        self.invalidate_display_view();
    }

    /// 取消旧派生任务并释放索引。
    pub(super) fn invalidate_display_view(&mut self) {
        self.cancel_display_view_build();
        self.display_view_request_seq = self.display_view_request_seq.wrapping_add(1);
        self.display_view_cache = None;
        self.display_view_build_key = None;
        self.display_view_building = false;
        self.display_view_error = None;
    }

    pub(super) fn cancel_display_view_build(&mut self) {
        if let Some(cancelled) = self.display_view_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn selected_rows(&self) -> &BTreeSet<usize> {
        &self.selected_rows
    }

    pub(super) fn toggle_row_selected(&mut self, ri: usize, cx: &mut Context<Self>) {
        if !self.selected_rows.remove(&ri) {
            self.selected_rows.insert(ri);
        }
        self.mark_selection_changed();
        cx.notify();
    }

    pub(super) fn toggle_visible_rows(&mut self, visible: &[usize], cx: &mut Context<Self>) {
        toggle_visible_selection(&mut self.selected_rows, visible);
        self.mark_selection_changed();
        cx.notify();
    }

    pub(super) fn clear_selected_rows(&mut self) {
        self.selected_rows.clear();
        self.mark_selection_changed();
    }

    pub(super) fn visible_selection_summary(&mut self, visible: &Arc<Vec<usize>>) -> (usize, bool) {
        if let Some(cache) = &self.visible_selection_cache
            && cache.selection_revision == self.selection_revision
            && Arc::ptr_eq(&cache.rows, visible)
        {
            return (
                cache.visible_selected,
                !visible.is_empty() && cache.visible_selected == visible.len(),
            );
        }
        let visible_selected = visible_selection_count(&self.selected_rows, visible);
        self.visible_selection_cache = Some(VisibleSelectionCache {
            rows: visible.clone(),
            selection_revision: self.selection_revision,
            visible_selected,
        });
        (
            visible_selected,
            !visible.is_empty() && visible_selected == visible.len(),
        )
    }

    fn mark_selection_changed(&mut self) {
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.visible_selection_cache = None;
    }

    pub(super) fn set_col_width_override(&mut self, col_ix: usize, width: gpui::Pixels) {
        let n_cols = match &self.state {
            ResultState::Ok(r) => r.columns.len(),
            _ => return,
        };
        if self.col_width_overrides.len() != n_cols {
            self.col_width_overrides.resize(n_cols, None);
        }
        if col_ix < self.col_width_overrides.len() {
            self.col_width_overrides[col_ix] = Some(width);
        }
    }

    pub(super) fn col_width_override(&self, col_ix: usize) -> Option<gpui::Pixels> {
        self.col_width_overrides.get(col_ix).copied().flatten()
    }

    pub(super) fn toggle_sort(&mut self, col_idx: usize, cx: &mut Context<Self>) {
        self.sort_by = match self.sort_by {
            Some((ci, SortDir::Asc)) if ci == col_idx => Some((col_idx, SortDir::Desc)),
            Some((ci, SortDir::Desc)) if ci == col_idx => None,
            _ => Some((col_idx, SortDir::Asc)),
        };
        self.selected_cell = None;
        self.invalidate_display_view();
        cx.notify();
    }

    pub(super) fn sort_by(&self) -> Option<(usize, SortDir)> {
        self.sort_by
    }

    pub(crate) fn pagination(&self) -> Option<ResultPagination> {
        self.pagination
    }

    pub(crate) fn set_pagination(
        &mut self,
        pagination: Option<ResultPagination>,
        cx: &mut Context<Self>,
    ) {
        if self.pagination == pagination {
            return;
        }
        self.pagination = pagination;
        cx.notify();
    }

    /// 仍有分页结果时回填精确总数。
    pub(crate) fn set_pagination_total(&mut self, total: TotalRows, cx: &mut Context<Self>) {
        let Some(pagination) = self.pagination.as_mut() else {
            return;
        };
        if pagination.total == total {
            return;
        }
        pagination.total = total;
        cx.notify();
    }

    pub(super) fn selected_cell(&self) -> Option<(usize, usize)> {
        self.selected_cell
    }

    pub(super) fn set_selected_cell(&mut self, cell: Option<(usize, usize)>) {
        self.selected_cell = cell;
    }

    pub(super) fn set_pending_notification(&mut self, n: Option<Notification>) {
        self.pending_notification = n;
    }

    pub fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.column_filter_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.row_filter_input
            .update(cx, |s, cx| s.set_value("", window, cx));
    }

    pub(super) fn column_filter_text(&self, cx: &gpui::App) -> String {
        self.column_filter_input.read(cx).value().trim().to_string()
    }

    pub(super) fn row_filter_text(&self, cx: &gpui::App) -> String {
        self.row_filter_input.read(cx).value().trim().to_string()
    }

    pub fn column_filter_entity(&self) -> &Entity<InputState> {
        &self.column_filter_input
    }
    pub fn row_filter_entity(&self) -> &Entity<InputState> {
        &self.row_filter_input
    }

    pub fn state(&self) -> &ResultState {
        &self.state
    }
}

fn global_memory_warning(outcome: ResultMemoryUpdate) -> Warning {
    let total_mib = outcome.total_bytes / 1024 / 1024;
    let message = if outcome.evicted_results > 0 {
        format!(
            "全部查询标签结果达到全局预算，已按 LRU 释放 {} 个非活动标签的旧结果；当前保留约 {total_mib} MiB",
            outcome.evicted_results
        )
    } else {
        format!(
            "全部查询标签结果已达到 384 MiB 提示线（当前约 {total_mib} MiB），建议关闭旧结果或收窄查询"
        )
    };
    Warning {
        level: "Client".into(),
        code: 0,
        message,
    }
}

impl Drop for ResultPanel {
    fn drop(&mut self) {
        self.cancel_display_view_build();
    }
}

/// 全选只作用于当前视图的源行索引，避免过滤 / 排序后误选其它原始行。
fn toggle_visible_selection(selected: &mut BTreeSet<usize>, visible: &[usize]) {
    let all_visible_selected =
        !visible.is_empty() && visible.iter().all(|ri| selected.contains(ri));
    if all_visible_selected {
        for ri in visible {
            selected.remove(ri);
        }
    } else {
        selected.extend(visible.iter().copied());
    }
}

fn visible_selection_count(selected: &BTreeSet<usize>, visible: &[usize]) -> usize {
    visible.iter().filter(|row| selected.contains(row)).count()
}

#[cfg(test)]
mod selection_tests {
    use super::{toggle_visible_selection, visible_selection_count};
    use std::collections::BTreeSet;

    #[test]
    fn filtered_select_all_uses_source_row_indices() {
        let mut selected = BTreeSet::new();

        toggle_visible_selection(&mut selected, &[2]);

        assert_eq!(selected, BTreeSet::from([2]));
    }

    #[test]
    fn toggling_visible_rows_preserves_hidden_selection() {
        let mut selected = BTreeSet::from([0, 2, 4]);

        toggle_visible_selection(&mut selected, &[2, 4]);

        assert_eq!(selected, BTreeSet::from([0]));
    }

    #[test]
    fn partial_visible_selection_selects_remaining_visible_rows() {
        let mut selected = BTreeSet::from([2]);

        toggle_visible_selection(&mut selected, &[2, 4]);

        assert_eq!(selected, BTreeSet::from([2, 4]));
    }

    #[test]
    fn visible_selection_count_ignores_hidden_rows() {
        let selected = BTreeSet::from([0, 2, 4]);

        assert_eq!(visible_selection_count(&selected, &[2, 3, 4]), 2);
    }
}
