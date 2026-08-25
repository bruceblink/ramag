mod export;
mod helpers;
mod ops;
mod render;
mod row_search;
mod scroll;

mod state;
use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use parking_lot::RwLock;

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, Point, ScrollHandle, ScrollStrategy,
    Subscription, UniformListScrollHandle, Window, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use ramag_app::ConnectionService;
use ramag_domain::entities::{
    Column, ConnectionConfig, MAX_SQL_QUERY_BYTES, QueryResult, TransactionId, Value, Warning,
};
use ramag_ui::{AxisScrollGesture, ResultMemoryLease, ResultMemoryUpdate};

use crate::sql_completion::SchemaCache;
use helpers::{PendingInsert, extract_first_table_ref, parse_value_for_kind};

pub(crate) use helpers::{RowIdentity, derive_row_identity};
use row_search::RowSearchState;
pub(crate) use row_search::{
    RowFilter, RowSearchBlocker, RowSearchConversionStatus, RowSearchMode,
};

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

/// 异步精确总行数状态；分页用哨兵判断下一页，COUNT(*) 后台回填。
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
    /// 精确总行数，跨页复用。
    pub(crate) total: TotalRows,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResultPanelEvent {
    PageRequested(usize),
    PageSizeChanged(usize),
    RowSearchChanged,
    RowFilterApply,
    MutationCompleted,
    MutationFailed(String),
}

impl EventEmitter<ResultPanelEvent> for ResultPanel {}

struct VisibleSelectionCache {
    rows: Arc<Vec<usize>>,
    selection_revision: u64,
    visible_selected: usize,
}

pub struct ResultPanel {
    pub(super) state: ResultState,
    pub(super) pending_notification: Option<Notification>,
    pub(super) selected_cell: Option<(usize, usize)>,
    /// 当前正在单元格内编辑的源行和列。
    pub(super) editing_cell: Option<(usize, usize)>,
    pub(super) selected_rows: BTreeSet<usize>,
    selection_revision: u64,
    visible_selection_cache: Option<VisibleSelectionCache>,
    pub(super) source_sql: Option<String>,
    pub(super) pinned_target: Option<(Option<String>, String)>,
    /// 行定位键（主键或全非空唯一索引）；未就绪时禁用行内修改和删除。
    pub(super) row_identity: Option<RowIdentity>,
    pub(super) col_width_overrides: Vec<Option<gpui::Pixels>>,
    pub(super) dml_busy: bool,
    /// Blocks generated mutations while a transaction is opening or finishing.
    pub(super) transaction_busy: bool,
    /// Driver-owned transaction used by generated row mutations.
    pub(super) transaction_id: Option<TransactionId>,
    pub(super) exporting: bool,
    pub(super) sort_by: Option<(usize, SortDir)>,
    /// 当前服务端结果页；None 表示不支持安全分页。
    pub(super) pagination: Option<ResultPagination>,
    /// 结果代际，供派生缓存和异步回包校验。
    pub(super) result_revision: u64,
    /// 排序、筛选和列布局缓存。
    pub(super) display_view_cache: Option<crate::views::result_table::DisplayViewCache>,
    /// 当前后台派生视图条件，避免重复排队。
    pub(super) display_view_build_key: Option<crate::views::result_table::DisplayViewCacheKey>,
    /// 派生视图构建状态与精确取消令牌。
    pub(super) display_view_building: bool,
    pub(super) display_view_cancel: Option<Arc<AtomicBool>>,
    pub(super) display_view_request_seq: u64,
    pub(super) display_view_error: Option<String>,
    pub(super) column_filter_input: Entity<InputState>,
    pub(super) row_filter_input: Entity<InputState>,
    row_search: RowSearchState,
    pub(super) cell_edit_input: Option<Entity<InputState>>,
    pub(super) cell_edit_subscription: Option<Subscription>,
    pub(super) service: Option<Arc<ConnectionService>>,
    pub(super) connection: Option<ConnectionConfig>,
    pub(super) schema_cache: Option<Arc<RwLock<SchemaCache>>>,
    pub(super) pending_insert: Option<PendingInsert>,
    pub(super) uniform_scroll: UniformListScrollHandle,
    pub(super) h_scroll: ScrollHandle,
    /// 结果表触控板手势的轴锁定状态。
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
            ramag_ui::bounded_search_input(window, cx).placeholder("WHERE 条件（按 Enter 执行）")
        });
        cx.observe(&column_filter_input, |_, _, cx| cx.notify())
            .detach();
        cx.observe(&row_filter_input, |this, _, cx| {
            this.on_row_filter_input_updated(cx);
        })
        .detach();
        cx.observe_global::<ramag_ui::DatabaseSearchSettingsGlobal>(|this, cx| {
            this.on_database_search_settings_changed(cx);
        })
        .detach();
        cx.observe_global::<ramag_ui::DatabaseResultSettingsGlobal>(|_, cx| cx.notify())
            .detach();

        cx.subscribe_in(
            &row_filter_input,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && matches!(this.row_search_mode(), RowSearchMode::Normal)
                    && !this.row_filter_text(cx).is_empty()
                {
                    cx.emit(ResultPanelEvent::RowFilterApply);
                    cx.notify();
                }
            },
        )
        .detach();
        Self {
            state: ResultState::Empty,
            pending_notification: None,
            selected_cell: None,
            editing_cell: None,
            source_sql: None,
            col_width_overrides: Vec::new(),
            dml_busy: false,
            transaction_busy: false,
            transaction_id: None,
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
            row_search: RowSearchState::default(),
            cell_edit_input: None,
            cell_edit_subscription: None,
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
            self.pending_insert = None;
            cx.notify();
        }
    }

    /// 注入精确目标表，避免 SQL 解析误差。
    pub fn set_pinned_target(&mut self, target: Option<(Option<String>, String)>) {
        self.pinned_target = target;
    }

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

    pub fn clear_editable_target(&mut self, cx: &mut Context<Self>) {
        if self.pinned_target.is_some() || self.row_identity.is_some() {
            self.pinned_target = None;
            self.row_identity = None;
            cx.notify();
        }
    }

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
            return Some("仅表树打开的单表可编辑");
        }
        if self.target_is_view() {
            return Some("视图不可写入");
        }
        None
    }

    pub(super) fn dml_busy(&self) -> bool {
        self.dml_busy
    }

    pub(crate) fn modify_block_reason(&self) -> Option<&'static str> {
        if let Some(r) = self.insert_block_reason() {
            return Some(r);
        }
        if self.row_identity.is_none() {
            return Some("该表无主键或全非空唯一索引，禁用行内修改 / 删除");
        }
        None
    }

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

    /// Updates the transaction context used by generated INSERT/UPDATE/DELETE statements.
    pub fn set_transaction(
        &mut self,
        transaction_id: Option<TransactionId>,
        busy: bool,
        cx: &mut Context<Self>,
    ) {
        self.transaction_id = transaction_id;
        self.transaction_busy = busy;
        cx.notify();
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

    /// 为可写单元格创建行内输入框；回车提交，失焦取消且不写库。
    pub(super) fn begin_cell_edit(
        &mut self,
        ri: usize,
        ci: usize,
        initial_text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_cell_edit_state();
        let input =
            cx.new(|cx_inner| InputState::new(window, cx_inner).default_value(initial_text));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => this.commit_cell_edit(cx),
                InputEvent::Blur => this.cancel_inline_cell_edit(cx),
                InputEvent::Change | InputEvent::Focus => {}
            },
        );
        self.editing_cell = Some((ri, ci));
        self.cell_edit_input = Some(input.clone());
        self.cell_edit_subscription = Some(subscription);
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// 取消行内编辑并释放输入框，避免结果刷新后继续持有旧编辑器。
    pub(super) fn cancel_inline_cell_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing_cell.is_none() {
            return;
        }
        self.clear_cell_edit_state();
        cx.notify();
    }

    /// 读取输入框文本并复用现有 UPDATE 安全链；校验失败时保留输入框供修正。
    fn commit_cell_edit(&mut self, cx: &mut Context<Self>) {
        let Some((ri, ci)) = self.editing_cell else {
            return;
        };
        let Some(input) = self.cell_edit_input.clone() else {
            return;
        };
        let new_text = input.read(cx).value().to_string();
        if self.apply_cell_update_async(ri, ci, new_text, cx) {
            self.clear_cell_edit_state();
            cx.notify();
        }
    }

    fn clear_cell_edit_state(&mut self) {
        self.cell_edit_subscription = None;
        self.cell_edit_input = None;
        self.editing_cell = None;
    }

    pub(super) fn set_cell_edit_input(&mut self, input: Option<Entity<InputState>>) {
        match input {
            Some(input) => self.cell_edit_input = Some(input),
            None => self.clear_cell_edit_state(),
        }
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

    pub(super) fn source_sql(&self) -> Option<String> {
        self.source_sql.clone()
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
        self.cancel_id_conversion();
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
mod selection_tests;
