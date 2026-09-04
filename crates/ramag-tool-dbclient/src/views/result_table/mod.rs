use std::ops::Range;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement, SharedString, div,
    prelude::*, px, uniform_list,
};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};

use ramag_domain::entities::{QueryResult, contains_case_insensitive};
use ramag_ui::RestrictScrollToAxisExt as _;

use super::result_panel::{
    MAX_ROWS_DISPLAY, ResultPanel, ResultPanelEvent, RowFilter, SortDir, TotalRows,
};

/// 连续输入筛选词时先等待短暂停顿，避免每个按键都占用共享 CPU 工作池。
const DISPLAY_VIEW_DEBOUNCE: Duration = Duration::from_millis(160);
/// 横向表格未做列虚拟化；限制交互式列数，避免异常宽结果创建数千个控件。
const MAX_COLUMNS_DISPLAY: usize = 512;

/// 单帧共享数据，供虚拟列表闭包读取。
struct TableRowFrame {
    result: Arc<QueryResult>,
    display_indices: Arc<Vec<usize>>,
    visible_col_indices: Arc<Vec<usize>>,
    col_widths: Vec<gpui::Pixels>,
    display_binary_16_as_uuid: bool,
    right_align: Arc<Vec<bool>>,
    row_number_offset: usize,
    row_num_width: gpui::Pixels,
    checkbox_col_width: gpui::Pixels,
    total_content_width: gpui::Pixels,
    mono_font: SharedString,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
    muted_bg: gpui::Hsla,
    accent: gpui::Hsla,
}

#[derive(Clone)]
pub(crate) struct DisplayView {
    pub(crate) visible_col_indices: Arc<Vec<usize>>,
    /// 列过滤命中总数；可能大于交互式显示上限。
    pub(crate) matched_col_count: usize,
    /// 是否因 MAX_COLUMNS_DISPLAY 仅显示命中列前缀。
    pub(crate) columns_truncated: bool,
    pub(crate) display_indices: Arc<Vec<usize>>,
    /// 基于当前显示行样本估算的默认列宽；手动覆盖在渲染时叠加。
    default_col_widths: Arc<Vec<gpui::Pixels>>,
    /// 基于当前显示行样本识别的数值列。
    right_align: Arc<Vec<bool>>,
    /// 是否因 MAX_ROWS_DISPLAY 截断未分页结果。
    pub(crate) truncated: bool,
    pub(crate) cols_filtered: bool,
    pub(crate) row_filtering: bool,
    pub(crate) pre_filter_count: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DisplayViewCacheKey {
    result_identity: usize,
    result_revision: u64,
    sort_by: Option<(usize, SortDir)>,
    column_filter: String,
    row_filter: RowFilter,
    display_binary_16_as_uuid: bool,
}

/// SQL 结果表派生视图缓存。内容不持有 QueryResult，避免延长旧结果的生命周期。
pub(crate) struct DisplayViewCache {
    key: DisplayViewCacheKey,
    view: DisplayView,
}

impl DisplayViewCache {
    fn get(&self, key: &DisplayViewCacheKey) -> Option<DisplayView> {
        (self.key == *key).then(|| self.view.clone())
    }
}

impl DisplayViewCacheKey {
    fn only_filters_differ_from(&self, previous: &Self) -> bool {
        self.result_identity == previous.result_identity
            && self.result_revision == previous.result_revision
            && self.sort_by == previous.sort_by
            && self.display_binary_16_as_uuid == previous.display_binary_16_as_uuid
            && (self.column_filter != previous.column_filter
                || self.row_filter != previous.row_filter)
    }
}

fn display_view_key(
    panel: &ResultPanel,
    result: &QueryResult,
    cx: &gpui::App,
) -> DisplayViewCacheKey {
    let column_filter = panel.column_filter_text(cx);
    let row_filter = panel.effective_row_filter(cx);
    let display_binary_16_as_uuid =
        ramag_ui::database_result_settings(cx).display_binary_16_as_uuid;
    DisplayViewCacheKey {
        result_identity: result as *const QueryResult as usize,
        result_revision: panel.result_revision,
        sort_by: panel.sort_by(),
        column_filter,
        row_filter,
        display_binary_16_as_uuid,
    }
}

/// 只读取已完成且与当前输入严格匹配的派生视图；用户操作不得同步回退扫描大结果集。
pub(crate) fn cached_display_view(
    panel: &ResultPanel,
    result: &QueryResult,
    cx: &gpui::App,
) -> Option<DisplayView> {
    let key = display_view_key(panel, result, cx);
    panel
        .display_view_cache
        .as_ref()
        .and_then(|cache| cache.get(&key))
}

/// 确保当前排序 / 筛选视图在受限工作池中计算。缓存未就绪时返回 None，渲染层显示进度态。
pub(super) fn ensure_display_view(
    panel: &mut ResultPanel,
    result: &Arc<QueryResult>,
    cx: &mut Context<ResultPanel>,
) -> Option<DisplayView> {
    let key = display_view_key(panel, result, cx);
    if let Some(view) = panel
        .display_view_cache
        .as_ref()
        .and_then(|cache| cache.get(&key))
    {
        return Some(view);
    }
    if panel.display_view_build_key.as_ref() == Some(&key) {
        return None;
    }

    let previous_key = panel
        .display_view_build_key
        .as_ref()
        .or_else(|| panel.display_view_cache.as_ref().map(|cache| &cache.key));
    let debounce = previous_key.is_some_and(|previous| key.only_filters_differ_from(previous));
    panel.cancel_display_view_build();
    panel.display_view_request_seq = panel.display_view_request_seq.wrapping_add(1);
    let request_seq = panel.display_view_request_seq;
    panel.display_view_cache = None;
    panel.display_view_build_key = Some(key.clone());
    panel.display_view_building = true;
    panel.display_view_error = None;
    let cancelled = Arc::new(AtomicBool::new(false));
    panel.display_view_cancel = Some(cancelled.clone());

    let result = result.clone();
    let request_key = key.clone();
    cx.spawn(async move |this, cx| {
        if debounce {
            cx.background_executor().timer(DISPLAY_VIEW_DEBOUNCE).await;
        }
        let current = this
            .update(cx, |this, _| {
                this.display_view_request_seq == request_seq
                    && this.display_view_build_key.as_ref() == Some(&request_key)
                    && this
                        .display_view_cancel
                        .as_ref()
                        .is_some_and(|token| Arc::ptr_eq(token, &cancelled))
            })
            .unwrap_or(false);
        if !current {
            return;
        }

        let worker_cancelled = cancelled.clone();
        let worker_key = request_key.clone();
        let built = ramag_app::run_blocking(move || {
            Ok(build_display_view_cancellable(
                &result,
                worker_key.sort_by,
                &worker_key.column_filter,
                &worker_key.row_filter,
                worker_key.display_binary_16_as_uuid,
                &worker_cancelled,
            ))
        })
        .await;
        let _ = this.update(cx, |this, cx| {
            if this.display_view_request_seq != request_seq
                || this.display_view_build_key.as_ref() != Some(&request_key)
                || !this
                    .display_view_cancel
                    .as_ref()
                    .is_some_and(|token| Arc::ptr_eq(token, &cancelled))
            {
                return;
            }
            this.display_view_cancel = None;
            this.display_view_building = false;
            match built {
                Ok(Some(view)) => {
                    this.display_view_cache = Some(DisplayViewCache {
                        key: request_key.clone(),
                        view,
                    });
                    this.display_view_build_key = None;
                }
                Ok(None) => {
                    this.display_view_build_key = None;
                }
                Err(error) => {
                    this.display_view_error = Some(format!("构建结果视图失败：{error}"));
                }
            }
            cx.notify();
        });
    })
    .detach();

    None
}

#[cfg(test)]
fn build_display_view(
    result: &QueryResult,
    sort_by: Option<(usize, SortDir)>,
    column_filter: &str,
    row_filter_lower: &str,
) -> DisplayView {
    let row_filter = RowFilter::Text(row_filter_lower.to_string());
    build_display_view_cancellable(
        result,
        sort_by,
        column_filter,
        &row_filter,
        true,
        &AtomicBool::new(false),
    )
    .expect("non-cancelled display view build should finish")
}

fn build_display_view_cancellable(
    result: &QueryResult,
    sort_by: Option<(usize, SortDir)>,
    column_filter: &str,
    row_filter: &RowFilter,
    display_binary_16_as_uuid: bool,
    cancelled: &AtomicBool,
) -> Option<DisplayView> {
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    let mut display_indices = (0..result.rows.len().min(MAX_ROWS_DISPLAY)).collect::<Vec<_>>();
    let truncated = result.rows.len() > MAX_ROWS_DISPLAY;

    if let Some((sort_col, dir)) = sort_by {
        display_indices.sort_by(|&a_index, &b_index| {
            if cancelled.load(Ordering::Relaxed) {
                return std::cmp::Ordering::Equal;
            }
            let a = &result.rows[a_index];
            let b = &result.rows[b_index];
            let av = a.values.get(sort_col);
            let bv = b.values.get(sort_col);
            let ord = compare_values(av, bv);
            if matches!(dir, SortDir::Desc) {
                ord.reverse()
            } else {
                ord
            }
        });
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
    }

    let col_tokens: Vec<String> = column_filter
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let cols_filtered = !col_tokens.is_empty();
    let matching_col_indices: Vec<usize> = if cols_filtered {
        let mut indices = Vec::new();
        for (index, column) in result.columns.iter().enumerate() {
            if index % 64 == 0 && cancelled.load(Ordering::Relaxed) {
                return None;
            }
            if col_tokens
                .iter()
                .any(|token| contains_case_insensitive(column, token))
            {
                indices.push(index);
            }
        }
        indices
    } else {
        (0..result.columns.len()).collect()
    };
    let pre_filter_count = display_indices.len();
    let row_filtering = row_filter.is_active();
    if row_filtering {
        let mut filtered = Vec::with_capacity(display_indices.len());
        for (position, source_idx) in display_indices.into_iter().enumerate() {
            if position % 64 == 0 && cancelled.load(Ordering::Relaxed) {
                return None;
            }
            let row = &result.rows[source_idx];
            if matching_col_indices.iter().any(|&ci| {
                row.values
                    .get(ci)
                    .map(|value| row_filter.matches(value))
                    .unwrap_or(false)
            }) {
                filtered.push(source_idx);
            }
        }
        display_indices = filtered;
    }

    let matched_col_count = matching_col_indices.len();
    let columns_truncated = matched_col_count > MAX_COLUMNS_DISPLAY;
    let visible_col_indices = matching_col_indices
        .into_iter()
        .take(MAX_COLUMNS_DISPLAY)
        .collect::<Vec<_>>();

    let mut default_col_widths = vec![px(100.0); result.columns.len()];
    let mut right_align = vec![false; result.columns.len()];
    for (position, &ci) in visible_col_indices.iter().enumerate() {
        if position % 16 == 0 && cancelled.load(Ordering::Relaxed) {
            return None;
        }
        default_col_widths[ci] = estimate_col_width(
            ci,
            &result.columns,
            &result.column_types,
            result,
            &display_indices,
            display_binary_16_as_uuid,
        );
        right_align[ci] = detect_numeric_column(ci, result, &display_indices);
    }

    Some(DisplayView {
        visible_col_indices: Arc::new(visible_col_indices),
        matched_col_count,
        columns_truncated,
        display_indices: Arc::new(display_indices),
        default_col_widths: Arc::new(default_col_widths),
        right_align: Arc::new(right_align),
        truncated,
        cols_filtered,
        row_filtering,
        pre_filter_count,
    })
}

mod cells;
mod page_size;
mod pagination;
mod render;
mod states;
mod toolbar;

pub(super) use page_size::render_page_size_selector;
pub(super) use render::render_table;
pub(super) use toolbar::render_result_view;
mod helpers;
#[cfg(test)]
mod render_test;

use cells::{render_data_row, render_header_cell, render_pending_row};
use helpers::{compare_values, detect_numeric_column, estimate_col_width};

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{Row, Value};

    fn sample_result() -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "name".into()],
            column_types: vec!["BIGINT".into(), "TEXT".into()],
            rows: vec![
                Row {
                    values: vec![Value::Int(2), Value::Text("Beta".into())],
                },
                Row {
                    values: vec![Value::Int(1), Value::Text("alpha".into())],
                },
                Row {
                    values: vec![Value::Int(3), Value::Text("Gamma".into())],
                },
            ],
            affected_rows: 0,
            elapsed_ms: 1,
            warnings: Vec::new(),
            truncated: false,
        }
    }

    #[test]
    fn display_view_keeps_source_indices_after_sort_and_filter() {
        let result = sample_result();
        let sorted = build_display_view(&result, Some((0, SortDir::Asc)), "", "");
        assert_eq!(sorted.display_indices.as_slice(), &[1, 0, 2]);

        let filtered = build_display_view(&result, Some((0, SortDir::Desc)), "name", "bet");
        assert_eq!(filtered.visible_col_indices.as_slice(), &[1]);
        assert_eq!(filtered.display_indices.as_slice(), &[0]);
        assert!(filtered.cols_filtered);
        assert!(filtered.row_filtering);
    }

    #[test]
    fn id_filter_matches_exact_integer_in_the_display_pipeline() {
        let result = sample_result();
        let filter = RowFilter::Integer(2);
        let view = build_display_view_cancellable(
            &result,
            None,
            "",
            &filter,
            true,
            &AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(view.display_indices.as_slice(), &[0]);
        assert!(view.row_filtering);
    }

    #[test]
    fn cancelled_display_view_stops_before_scanning_rows() {
        let cancelled = AtomicBool::new(true);
        let row_filter = RowFilter::Text("alpha".into());
        assert!(
            build_display_view_cancellable(
                &sample_result(),
                Some((0, SortDir::Asc)),
                "name",
                &row_filter,
                true,
                &cancelled,
            )
            .is_none()
        );
    }

    #[test]
    fn display_view_cache_reuses_only_matching_revision_and_inputs() {
        let view = build_display_view(&sample_result(), None, "", "");
        let key = DisplayViewCacheKey {
            result_identity: 7,
            result_revision: 3,
            sort_by: None,
            column_filter: String::new(),
            row_filter: RowFilter::Text(String::new()),
            display_binary_16_as_uuid: true,
        };
        let cache = DisplayViewCache {
            key: key.clone(),
            view,
        };

        let cached = cache.get(&key).expect("same cache key should hit");
        assert!(Arc::ptr_eq(
            &cached.display_indices,
            &cache.view.display_indices
        ));

        let mut stale = key;
        stale.result_revision += 1;
        assert!(cache.get(&stale).is_none());

        let mut display_changed = cache.key.clone();
        display_changed.display_binary_16_as_uuid = false;
        assert!(cache.get(&display_changed).is_none());
    }

    #[test]
    fn mixed_and_json_values_have_deterministic_direct_ordering() {
        let values = [
            Value::Json(serde_json::json!({"z": [3, 2, 1]})),
            Value::Text("plain".into()),
            Value::Bool(true),
        ];
        for (left_index, left) in values.iter().enumerate() {
            for (right_index, right) in values.iter().enumerate() {
                let forward = helpers::compare_values(Some(left), Some(right));
                let reverse = helpers::compare_values(Some(right), Some(left));
                assert_eq!(forward, reverse.reverse(), "{left_index} vs {right_index}");
            }
        }
        assert_eq!(
            helpers::compare_values(
                Some(&Value::Json(serde_json::json!([1, 2]))),
                Some(&Value::Json(serde_json::json!([1, 3]))),
            ),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn column_filter_matches_unicode_case_insensitively() {
        let mut result = sample_result();
        result.columns[1] = "ÜBERblick".into();

        let view = build_display_view(&result, None, "über", "");

        assert_eq!(view.visible_col_indices.as_slice(), &[1]);
    }

    #[test]
    fn wide_results_cap_rendered_columns_but_filter_all_columns() {
        let column_count = MAX_COLUMNS_DISPLAY + 1;
        let mut values = vec![Value::Text(String::new()); column_count];
        values[column_count - 1] = Value::Text("needle".into());
        let result = QueryResult {
            columns: (0..column_count).map(|index| format!("c{index}")).collect(),
            column_types: vec!["TEXT".into(); column_count],
            rows: vec![Row { values }],
            affected_rows: 0,
            elapsed_ms: 1,
            warnings: Vec::new(),
            truncated: false,
        };

        let view = build_display_view(&result, None, "", "needle");

        assert_eq!(view.visible_col_indices.len(), MAX_COLUMNS_DISPLAY);
        assert_eq!(view.matched_col_count, column_count);
        assert!(view.columns_truncated);
        assert_eq!(view.display_indices.as_slice(), &[0]);
    }
}
