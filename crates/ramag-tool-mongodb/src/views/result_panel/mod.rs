//! 结果展示：表格视图（只解析第一层字段；嵌套对象/数组单元格摘要化，双击看完整）
//! - 顶部 toolbar：过滤列 + 过滤行 + 增删 + 导出
//! - 主体：uniform_list 行级虚拟化的表格（列头 + 行）
//! - 单元格双击：标量编辑 / 嵌套看完整 JSON

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
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement, Point, Render,
    ScrollHandle, SharedString, Styled, UniformListScrollHandle, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use parking_lot::RwLock;
use ramag_app::MongoService;
use ramag_domain::entities::{ConnectionConfig, MongoQueryResult};
use serde_json::Value;

pub use flatten::FlatTable;

use filter::{ParsedFilter, classify_filter, column_indices_for, row_indices_for};

/// 过滤列补全收集的最大嵌套深度（支持 consume.detail.x 这类多层）
const PATH_COMPLETION_DEPTH: usize = 5;

pub struct ResultPanel {
    pub(crate) result: Option<MongoQueryResult>,
    /// 当前层文档的唯一共享所有权；查询结果 DTO 中的 documents 在 set_result 时移入这里。
    pub(crate) docs_arc: Option<Arc<Vec<serde_json::Value>>>,
    pub(crate) error: Option<String>,
    pub(crate) running: bool,
    /// 扁平化表格视图（result 变化时重算）
    pub(crate) table: Option<Arc<FlatTable>>,
    /// 工具栏：过滤列名（逗号分隔多列）
    pub(crate) column_filter: Entity<InputState>,
    /// 工具栏：过滤行（子串包含；任意单元格匹配即保留）
    pub(crate) row_filter: Entity<InputState>,
    /// 表格虚拟列表纵滚句柄（uniform_list 内部 Y）
    pub(crate) uniform_scroll: UniformListScrollHandle,
    /// 横滚句柄（外层 div X；与 dbclient::result_table 同模式）
    pub(crate) h_scroll: ScrollHandle,
    /// 列过滤框补全候选源（set_result 时填入当前结果集列 path）
    pub(crate) column_completion_source: Arc<RwLock<Vec<String>>>,
    /// DML 执行上下文（由 query_tab 注入；None 时禁用增删改）
    pub(crate) service: Option<Arc<MongoService>>,
    pub(crate) config: Option<ConnectionConfig>,
    pub(crate) database: String,
    /// 当前结果对应的 collection（run 时从命令提取，是增删改的目标）
    pub(crate) target_collection: Option<String>,
    /// 异步 DML 完成后挂起的 toast，下次 render 推送
    pub(crate) pending_notification: Option<gpui_component::notification::Notification>,
    /// 文档 DML（插入 / 编辑弹框）忙碌闸：提交中防重入；失败保留弹框与输入
    pub(super) doc_dml_busy: bool,
    /// DML 成功后待关闭弹框（render 持 Window 时消费）——成功才关，失败留着改
    pub(super) pending_close_dialog: bool,
    /// 勾选的行（按 documents 索引）；删除文档用
    pub(crate) selected_rows: BTreeSet<usize>,
    /// 下钻栈：栈底=原始查询结果，双击嵌套 push 一层；栈深 > 1 即下钻态（只读 + 面包屑）
    pub(crate) drill_stack: Vec<drill::DrillLevel>,
    /// 当前排序列 path + 方向；用 path 而非索引，钻取换表后失配自动失效
    pub(crate) sort_by: Option<(String, SortDir)>,
    _subscriptions: Vec<gpui::Subscription>,
}

/// 结果区事件：DML 成功后请求 query_tab 重跑当前命令以刷新结果
#[derive(Clone, Debug)]
pub enum ResultEvent {
    Refresh,
    /// 仅停止客户端等待；MongoDB 服务端操作可能仍在执行。
    Cancel,
}

/// 排序方向（单击列头切换 None→Asc→Desc→None）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortDir {
    Asc,
    Desc,
}

impl EventEmitter<ResultEvent> for ResultPanel {}

impl ResultPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 列过滤补全源：set_result 时填入当前结果集列 path，供 ColumnFilterCompletionProvider 读取
        let column_completion_source: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
        let provider = crate::completion::ColumnFilterCompletionProvider::new_rc(
            column_completion_source.clone(),
        );
        let column_filter = cx.new(|cx| {
            let mut state =
                InputState::new(window, cx).placeholder("过滤列（列名或路径，逗号分隔）");
            state.lsp.completion_provider = Some(provider);
            state
        });
        let row_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("过滤行（任意单元格包含）"));

        let subs = vec![
            cx.subscribe(&column_filter, |_this, _, _e: &InputEvent, cx| {
                // 基础表不变；补全源在 rebuild 时已就绪，仅重渲染
                cx.notify();
            }),
            cx.subscribe(&row_filter, |_this, _, _e: &InputEvent, cx| cx.notify()),
        ];

        Self {
            result: None,
            docs_arc: None,
            error: None,
            running: false,
            table: None,
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
            pending_close_dialog: false,
            selected_rows: BTreeSet::new(),
            drill_stack: Vec::new(),
            sort_by: None,
            _subscriptions: subs,
        }
    }

    /// 注入 DML 执行上下文（连接 + 默认库）；由 query_tab::new 调
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

    /// 设置当前结果对应的 collection（增删改目标）；run 提取命令后调
    pub fn set_target_collection(&mut self, coll: Option<String>) {
        self.target_collection = coll;
    }

    /// 同步当前 db：切库 / 切 collection 后写操作（update/delete/insert）要落到正确的库，
    /// 否则沿用 tab 初始库、filter 匹配不到文档（matched 0）→ 表现为「更新不了」
    pub fn set_database(&mut self, db: String) {
        self.database = db;
    }

    /// 切库后旧结果绝不能继续作为新库的 DML 上下文；有结果时明确标为失效。
    pub fn switch_database(&mut self, db: String, cx: &mut Context<Self>) {
        let had_result = self.running || self.result.is_some() || self.error.is_some();
        self.database = db;
        self.target_collection = None;
        self.selected_rows.clear();
        if had_result {
            self.set_error("已切换数据库，旧结果已失效；请重新运行命令".into(), cx);
        } else {
            cx.notify();
        }
    }

    /// 清空列 / 行过滤框：切换 collection（换数据源）时调，避免旧过滤词残留串到新结果
    pub fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.column_filter
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.row_filter
            .update(cx, |s, cx| s.set_value("", window, cx));
    }

    /// 当前连接是否生产只读（driver 层拦截写操作；前端同步禁用写入口）
    pub(crate) fn is_production(&self) -> bool {
        self.config.as_ref().is_some_and(|c| c.production)
    }

    /// 能否写（增删改）：上下文齐全 + 有目标 collection + 非生产只读
    pub(crate) fn can_write(&self) -> bool {
        self.service.is_some()
            && self.config.is_some()
            && self.target_collection.is_some()
            && !self.is_production()
    }

    /// 切换某行勾选（按 documents 索引）
    pub(crate) fn toggle_row(&mut self, idx: usize, cx: &mut Context<Self>) {
        if !self.selected_rows.insert(idx) {
            self.selected_rows.remove(&idx);
        }
        cx.notify();
    }

    /// 全选 / 全不选（传入当前可见的全部行索引）
    pub(crate) fn toggle_all(&mut self, all: &[usize], cx: &mut Context<Self>) {
        if !all.is_empty() && all.iter().all(|i| self.selected_rows.contains(i)) {
            for i in all {
                self.selected_rows.remove(i);
            }
        } else {
            self.selected_rows.extend(all.iter().copied());
        }
        cx.notify();
    }

    pub(crate) fn is_row_selected(&self, idx: usize) -> bool {
        self.selected_rows.contains(&idx)
    }

    /// 单击列头切换排序：同列 None→Asc→Desc→None；换列直接 Asc
    pub(crate) fn toggle_sort(&mut self, path: String, cx: &mut Context<Self>) {
        self.sort_by = match self.sort_by.take() {
            Some((p, SortDir::Asc)) if p == path => Some((path, SortDir::Desc)),
            Some((p, SortDir::Desc)) if p == path => None,
            _ => Some((path, SortDir::Asc)),
        };
        cx.notify();
    }

    pub fn set_running(&mut self, cx: &mut Context<Self>) {
        self.running = true;
        self.error = None;
        cx.notify();
    }

    /// 生产只读拦截（Forbidden）后复位忙碌态并恢复拦截前的错误文案：
    /// 旧结果 / 旧错误原样保留，结果区绝不停留在"执行中"
    pub fn restore_idle(&mut self, prev_error: Option<String>, cx: &mut Context<Self>) {
        self.running = false;
        self.error = prev_error;
        cx.notify();
    }

    pub fn set_result(&mut self, mut r: MongoQueryResult, cx: &mut Context<Self>) {
        self.selected_rows.clear();
        // 新查询：重置下钻栈为顶层（label 用目标 collection）
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
        // 切结果时把横滚归位最左（与 dbclient::result_table 同款），避免新表格沿用旧的横滚 X 位置
        self.h_scroll.set_offset(Point::new(px(0.0), px(0.0)));
        // 建基础表 + 刷新补全源
        self.rebuild_table();
        cx.notify();
    }

    pub fn set_error(&mut self, err: String, cx: &mut Context<Self>) {
        self.error = Some(err);
        self.running = false;
        cx.notify();
    }

    /// 解析过滤列框；规则见 classify_filter。
    pub(crate) fn parse_column_filter(&self, cx: &gpui::App) -> ParsedFilter {
        let raw = self.column_filter.read(cx).value().to_string();
        classify_filter(&raw)
    }

    /// 重建基础表格（不钻取）与补全源；钻取/投影在 render 时按过滤框派生
    pub(crate) fn rebuild_table(&mut self) {
        let level = self.drill_stack.last();
        let docs = level
            .map(|l| l.documents.clone())
            .unwrap_or_else(|| Arc::new(Vec::new()));
        let ancestors = level.map(|l| l.ancestors.clone()).unwrap_or_default();
        self.table = if docs.is_empty() {
            None
        } else {
            let mut ft = flatten::build_flat_table_with(docs.as_slice(), &BTreeSet::new());
            // 下钻层：祖先链为常量（该层所有行同一父），作前导列，根→深保序，列名即对象名
            if !ancestors.is_empty() {
                let lead_cols: Vec<flatten::Column> = ancestors
                    .iter()
                    .map(|(label, c)| flatten::Column {
                        path: label.clone(),
                        kind: if c.kind == "null" { "text" } else { c.kind },
                    })
                    .collect();
                let lead_cells: Vec<_> = ancestors.iter().map(|(_, c)| c.clone()).collect();
                let lead_rows = vec![lead_cells; ft.rows.len()];
                ft.prepend_lead(lead_cols, lead_rows);
            }
            Some(Arc::new(ft))
        };
        *self.column_completion_source.write() =
            flatten::collect_paths(docs.as_slice(), PATH_COMPLETION_DEPTH);
    }

    /// 当前过滤后的列索引（None 表示全选）；用所有 token 子串匹配（unwind 锚不匹配子列、自然不影响）
    pub(crate) fn filtered_column_indices(&self, cx: &gpui::App) -> Option<Vec<usize>> {
        column_indices_for(self.table.as_ref()?, &self.parse_column_filter(cx).filters)
    }

    /// 当前过滤后的行索引；空过滤串等价 None
    pub(crate) fn filtered_row_indices(&self, cx: &gpui::App) -> Option<Vec<usize>> {
        let raw = self.row_filter.read(cx).value().to_string();
        row_indices_for(self.table.as_ref()?, &raw)
    }

    /// 双击单元格 → 弹该单元格内容详情（与 MySQL dbclient::cell_edit_dialog 同款交互）。
    /// 标题是「{列名} ({类型})」；内容若是合法 JSON 自动 pretty 格式化
    pub(crate) fn open_cell_dialog(
        &self,
        column_path: String,
        kind: &'static str,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display = if text.starts_with('{') || text.starts_with('[') {
            serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| text.clone())
        } else {
            text.clone()
        };
        let title: SharedString = SharedString::from(format!("{column_path}  ({kind})"));
        let input: Entity<InputState> = cx.new(|cx_inner| {
            InputState::new(window, cx_inner)
                .multi_line(true)
                .default_value(display)
        });
        window.open_dialog(cx, move |dialog, _w, _app| {
            let input = input.clone();
            let title = title.clone();
            dialog
                .title(title)
                .close_button(true)
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

impl Render for ResultPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 异步 DML 完成的 toast 在这里推送
        if let Some(n) = self.pending_notification.take() {
            window.push_notification(n, cx);
        }
        // DML 成功后才关闭插入 / 编辑弹框（失败保留弹框与用户输入）
        if std::mem::take(&mut self.pending_close_dialog) {
            window.close_dialog(cx);
        }
        // 把需要的颜色字段提前 Copy 出来，避免 cx.theme() 的 immut borrow 与 toolbar/table 的 mut borrow 冲突
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
        // 借用而非 clone：render 只用 affected / documents.len() / elapsed_ms / truncated，
        // 每帧深拷贝整个 MongoQueryResult（含万级文档）纯属浪费
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
            let hint = if affected > 0 {
                format!("已执行写操作，影响 {affected} 条")
            } else if self.is_drilled() {
                "（空）".to_string()
            } else {
                "（无文档返回）".to_string()
            };
            // 下钻到空数据也要保留面包屑（toolbar 下方），否则无法返回上层
            let mut root = v_flex().size_full().bg(bg).child(toolbar::render(self, cx));
            if self.is_drilled() {
                root = root.child(self.render_breadcrumb(cx));
            }
            return root.child(empty_hint(hint, muted)).into_any_element();
        };

        let col_indices = self.filtered_column_indices(cx);
        let row_indices = self.filtered_row_indices(cx);
        let filtered_rows = row_indices.as_ref().map(|v| v.len()).unwrap_or(total_docs);
        let visible_selected = row_indices
            .as_ref()
            .map_or(self.selected_rows.len(), |rows| {
                rows.iter()
                    .filter(|ri| self.selected_rows.contains(ri))
                    .count()
            });
        let hidden_selected = self.selected_rows.len().saturating_sub(visible_selected);
        let total_cols = self.table.as_ref().map(|t| t.columns.len()).unwrap_or(0);
        let visible_cols_count = col_indices.as_ref().map(|v| v.len()).unwrap_or(total_cols);
        let mut summary = match (row_indices.is_some(), col_indices.is_some()) {
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

        // toolbar（搜索栏）始终在顶、位置不变；下钻态时在其下方插入面包屑栏
        let mut root = v_flex()
            .size_full()
            .bg(bg)
            .child(toolbar::render(self, cx))
            .child(div().h(px(1.0)).bg(border));
        // 截断横幅：结果被安全上限截断时醒目提示，避免用户误以为看到 / 导出的是全量
        if truncated {
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
                        "⚠ 结果较大，仅加载前 {total_docs} 条；统计、排序、过滤与导出均基于这部分数据。请用 filter / limit 精确查询"
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
            None,
            true,
            cx,
        )))
        .child(render_status_bar(summary, border, muted, bg))
        .into_any_element()
    }
}

/// 底部 status bar：行数 / 耗时 / 过滤命中数（与 dbclient::result_table 同款）
fn render_status_bar(
    summary: String,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    bg: gpui::Hsla,
) -> impl IntoElement {
    div()
        .id("mongo-status-bar")
        .w_full()
        .flex_none()
        .px(px(12.0))
        .py(px(4.0))
        .border_t_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .text_color(muted)
        .child(SharedString::from(summary))
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
