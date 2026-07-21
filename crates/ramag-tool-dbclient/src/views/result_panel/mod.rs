//! 结果集面板：Empty / Running / Error / Ok 四态

mod export;
mod helpers;
mod ops;
mod render;

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
use ramag_domain::entities::{Column, ConnectionConfig, MAX_SQL_QUERY_BYTES, QueryResult, Value};

use crate::sql_completion::SchemaCache;
use helpers::{PendingInsert, extract_first_table_ref, parse_value_for_kind};

// QueryTab 在查询成功后据元数据推导行定位键并注入
pub(crate) use helpers::{RowIdentity, derive_row_identity};

/// 服务端分页的可见页大小，也是未分页结果的 UI 渲染上限。
pub(super) const MAX_ROWS_DISPLAY: usize = 10_000;
/// 行内新增最多创建的输入框数量，避免异常元数据一次生成数万控件。
pub(super) const MAX_INSERT_COLUMNS: usize = 512;

/// 结果集状态
#[derive(Debug, Clone, Default)]
pub enum ResultState {
    #[default]
    Empty,
    Running,
    Error(String),
    Ok(Arc<QueryResult>),
}

/// 排序方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// 全表精确总行数的异步计数状态。分页刻意只用哨兵判断有无下一页，
/// 精确总数由首屏后台 COUNT(*) 回填，故需三态区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TotalRows {
    /// 计数进行中，底栏显示“计算中…”
    Counting,
    /// 已知精确总数
    Known(u64),
    /// 计数失败或不可用，底栏留空
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
    /// 异步任务（如导出）完成后挂这里，下次 render 在 window 上下文里 push
    pub(super) pending_notification: Option<Notification>,
    /// 当前选中单元格 (row_idx, col_idx)，用于高亮 + cmd-c 复制
    pub(super) selected_cell: Option<(usize, usize)>,
    /// 多选行：表格首列 checkbox 勾选的行索引集合
    pub(super) selected_rows: BTreeSet<usize>,
    /// 选择变化代次与当前可见行交集缓存，避免普通重渲染反复扫描最多一万行。
    selection_revision: u64,
    visible_selection_cache: Option<VisibleSelectionCache>,
    /// 当前结果对应的源 SQL（QueryTab 在 run/explain 后注入）
    pub(super) source_sql: Option<String>,
    /// 上游显式注入的目标 (schema, table)：表树点击时由 QueryPanel 传入
    pub(super) pinned_target: Option<(Option<String>, String)>,
    /// 行定位键（真实主键 / 全非空唯一索引）：QueryTab 查询成功后异步注入；
    /// None = 元数据未就绪或该表无键，行内修改 / 删除一律禁用
    pub(super) row_identity: Option<RowIdentity>,
    /// 列宽手动覆盖：用户拖动列分隔线后写入
    pub(super) col_width_overrides: Vec<Option<gpui::Pixels>>,
    /// DML（增删改）防重入闸：spawn 前置位、回包复位；置位期间再次提交被 dml_conn 拦下
    pub(super) dml_busy: bool,
    /// 导出防重入闸；后台任务完成（含取消/失败）后复位。
    pub(super) exporting: bool,
    /// 当前排序列与方向：单击列头切换 None→Asc→Desc→None
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
    /// 列过滤输入框：逗号分隔多列名（命中即显示该列）
    pub(super) column_filter_input: Entity<InputState>,
    /// 行过滤输入框：单一关键字
    pub(super) row_filter_input: Entity<InputState>,
    /// 单元格编辑弹框输入框：保活引用
    pub(super) cell_edit_input: Option<Entity<InputState>>,
    /// 行内编辑用的执行器（由 QueryTab 注入）
    pub(super) service: Option<Arc<ConnectionService>>,
    pub(super) connection: Option<ConnectionConfig>,
    /// 表元数据 cache（由 QueryTab 注入）：用于禁用视图上的写按钮
    pub(super) schema_cache: Option<Arc<RwLock<SchemaCache>>>,
    /// 新增草稿行：表格末尾追加可编辑空行
    pub(super) pending_insert: Option<PendingInsert>,
    /// 结果表格虚拟列表的垂直滚动句柄
    pub(super) uniform_scroll: UniformListScrollHandle,
    /// 外层水平滚动 div 的 ScrollHandle
    pub(super) h_scroll: ScrollHandle,
    /// 列过滤框的补全候选源
    pub(super) column_completion_source: Arc<RwLock<Vec<String>>>,
    /// SHOW WARNINGS 面板是否展开
    pub(super) warnings_expanded: bool,
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
        // 输入变化 → 触发 ResultPanel 重渲染
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
            column_completion_source,
            warnings_expanded: false,
        }
    }

    pub(super) fn uniform_scroll(&self) -> &UniformListScrollHandle {
        &self.uniform_scroll
    }

    pub(super) fn h_scroll(&self) -> &ScrollHandle {
        &self.h_scroll
    }

    /// 进入新增模式：表格末尾追加可编辑草稿行（DataGrip 风格）
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

    /// 提交新增：遍历每列 InputState 校验后调 apply_insert_async
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

    /// 上游（QueryTab.run）注入精确目标表，避免 SQL parse 误差
    pub fn set_pinned_target(&mut self, target: Option<(Option<String>, String)>) {
        self.pinned_target = target;
    }

    /// 注入行定位键，但仅当结果集仍对应该目标表（防慢回包串到新查询）
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

    /// 用户手改 SQL 后立即失去"表树单表数据"资格：清目标表与定位键，结果区转只读
    pub fn clear_editable_target(&mut self, cx: &mut Context<Self>) {
        if self.pinned_target.is_some() || self.row_identity.is_some() {
            self.pinned_target = None;
            self.row_identity = None;
            cx.notify();
        }
    }

    /// 行内新增被禁用的原因；None = 可新增（INSERT 不依赖行定位键）
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

    /// 行内修改 / 删除被禁用的原因；None = 允许（比新增额外要求行定位键）
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

    /// 当前结果集对应的目标是否视图（视图禁止 INSERT/UPDATE/DELETE）
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

    /// 单元格编辑弹框的只读原因：写入闸门未过 → 相应原因；二进制单元格 → 防损坏只读。
    /// None = 可编辑提交
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

    /// 行定位方式的提示文案（"主键" / "唯一键"）；编辑弹框展示用
    pub(super) fn identity_label(&self) -> &'static str {
        self.row_identity
            .as_ref()
            .map(|i| i.label)
            .unwrap_or("主键")
    }

    /// 删除 / 预览用的展示列：行定位键第一列在结果集中的下标，无键回退第 0 列
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

    /// 该单元格是否为二进制值。二进制显示的是 hex 文本，编辑保存会把它写成 hex 的
    /// ASCII 文本（损坏原始字节），故弹框强制只读（可查看 / 复制，不能提交）
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
        // 数据集变更后清除选中、排序、列宽覆盖、新增草稿
        self.selected_cell = None;
        self.clear_selected_rows();
        self.sort_by = None;
        self.col_width_overrides.clear();
        self.pending_insert = None;
        // 客户端资源警告直接展开，避免用户把已截断结果误认为完整结果。
        self.warnings_expanded = has_client_warning;
        // 行定位键跟随结果集：新结果由 QueryTab 在查询成功后重新拉元数据注入
        self.row_identity = None;
        // 切表/重跑时双向归位：垂直回顶 + 水平回左
        self.uniform_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.h_scroll.set_offset(Point::new(px(0.0), px(0.0)));
        cx.notify();
    }

    /// 恢复被 Running 覆盖前的状态快照（生产只读拦截 Forbidden 时用）：
    /// 不走 set_state 的清理逻辑，选中 / 排序 / 滚动位置全部保持原样
    pub fn restore_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
        if let ResultState::Ok(qr) = &state {
            *self.column_completion_source.write() = qr.columns.clone();
        }
        self.state = state;
        self.mark_result_changed();
        cx.notify();
    }

    /// 标记结果数据已变化，并丢弃所有依赖旧行内容的派生缓存。
    pub(super) fn mark_result_changed(&mut self) {
        self.result_revision = self.result_revision.wrapping_add(1);
        self.invalidate_display_view();
    }

    /// 排序、筛选或结果变化后取消旧 CPU 任务并释放派生索引。
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

    /// 后台 COUNT 返回后回填精确总数；当前无分页结果时忽略（如已被新查询清空）。
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
