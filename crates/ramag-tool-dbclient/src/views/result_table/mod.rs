//! 结果集表格：uniform_list 行级虚拟化，受 driver LIMIT 与 MAX_ROWS_DISPLAY 限制

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement, SharedString, Styled,
    div, prelude::*, px, uniform_list,
};

/// 禁用 GPUI 单轴 scroll 的"另一方向劫持"，wheel 严格按方向消费
trait RestrictScrollExt: Styled + Sized {
    fn restrict_scroll_to_axis(mut self) -> Self {
        self.style().restrict_scroll_to_axis = Some(true);
        self
    }
}
impl<T: Styled> RestrictScrollExt for T {}
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex, v_flex,
};

use ramag_domain::entities::{QueryResult, contains_case_insensitive};

use super::result_panel::{MAX_ROWS_DISPLAY, ResultPanel, SortDir};

/// 帧级数据：本次 render_table 计算一次，供 uniform_list closure 共享访问
/// 用 Rc 包装才能在 'static + Fn 闭包内 capture（不能 borrow 栈局部变量）
struct TableRowFrame {
    result: Arc<QueryResult>,
    /// 排序 + 过滤后的源行下标；行数据始终从共享 result 读取，不在每帧复制。
    display_indices: Arc<Vec<usize>>,
    visible_col_indices: Arc<Vec<usize>>,
    col_widths: Vec<gpui::Pixels>,
    right_align: Arc<Vec<bool>>,
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

/// 表格当前视图（排序 + 列/行过滤后的所见内容）；渲染与导出共用，保证所见即所导
#[derive(Clone)]
pub(crate) struct DisplayView {
    /// 可见列的原始下标（列过滤后）
    pub(crate) visible_col_indices: Arc<Vec<usize>>,
    /// 原始行下标：排序 + 行过滤后的显示序
    pub(crate) display_indices: Arc<Vec<usize>>,
    /// 基于当前显示行样本估算的默认列宽；手动覆盖在渲染时叠加。
    default_col_widths: Arc<Vec<gpui::Pixels>>,
    /// 基于当前显示行样本识别的数值列。
    right_align: Arc<Vec<bool>>,
    /// 是否因 MAX_ROWS_DISPLAY 截断
    pub(crate) truncated: bool,
    /// 列过滤是否激活
    pub(crate) cols_filtered: bool,
    /// 行过滤是否激活
    pub(crate) row_filtering: bool,
    /// 行过滤前的行数（显示"过滤 N/M"用）
    pub(crate) pre_filter_count: usize,
}

#[derive(Clone, PartialEq, Eq)]
struct DisplayViewCacheKey {
    result_identity: usize,
    result_revision: u64,
    sort_by: Option<(usize, SortDir)>,
    column_filter: String,
    row_filter_lower: String,
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

impl DisplayView {
    /// 视图是否与原始结果不同（有排序 / 过滤）——导出时据此决定导原始还是导视图
    pub(crate) fn differs_from_raw(&self, panel: &ResultPanel) -> bool {
        self.cols_filtered || self.row_filtering || panel.sort_by().is_some()
    }
}

/// 计算表格当前视图：保留原始行下标供 DML / 复制 / 编辑定位真实行；
/// 排序、行过滤都在 (source_idx, row) 对上进行（仅前 MAX_ROWS_DISPLAY 行）
pub(crate) fn compute_display_view(
    panel: &ResultPanel,
    result: &QueryResult,
    cx: &gpui::App,
) -> DisplayView {
    let column_filter = panel.column_filter_text(cx);
    let row_filter_lower = panel.row_filter_text(cx).to_lowercase();
    let key = DisplayViewCacheKey {
        result_identity: result as *const QueryResult as usize,
        result_revision: panel.result_revision,
        sort_by: panel.sort_by(),
        column_filter: column_filter.clone(),
        row_filter_lower: row_filter_lower.clone(),
    };
    if let Some(view) = panel
        .display_view_cache
        .borrow()
        .as_ref()
        .and_then(|cache| cache.get(&key))
    {
        return view;
    }

    let view = build_display_view(result, panel.sort_by(), &column_filter, &row_filter_lower);
    panel.display_view_cache.replace(Some(DisplayViewCache {
        key,
        view: view.clone(),
    }));
    view
}

fn build_display_view(
    result: &QueryResult,
    sort_by: Option<(usize, SortDir)>,
    column_filter: &str,
    row_filter_lower: &str,
) -> DisplayView {
    let mut display_indices = (0..result.rows.len().min(MAX_ROWS_DISPLAY)).collect::<Vec<_>>();
    let truncated = result.rows.len() > MAX_ROWS_DISPLAY;

    if let Some((sort_col, dir)) = sort_by {
        display_indices.sort_by(|&a_index, &b_index| {
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
    }

    let col_tokens: Vec<String> = column_filter
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let cols_filtered = !col_tokens.is_empty();
    let visible_col_indices: Vec<usize> = if cols_filtered {
        result
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| {
                col_tokens
                    .iter()
                    .any(|token| contains_case_insensitive(column, token))
            })
            .map(|(i, _)| i)
            .collect()
    } else {
        (0..result.columns.len()).collect()
    };
    let pre_filter_count = display_indices.len();
    let row_filtering = !row_filter_lower.is_empty();
    if row_filtering {
        display_indices.retain(|&source_idx| {
            let row = &result.rows[source_idx];
            visible_col_indices.iter().any(|&ci| {
                row.values
                    .get(ci)
                    .map(|value| value.contains_query_lower(row_filter_lower))
                    .unwrap_or(false)
            })
        });
    }

    let default_col_widths = (0..result.columns.len())
        .map(|ci| {
            estimate_col_width(
                ci,
                &result.columns,
                &result.column_types,
                result,
                &display_indices,
            )
        })
        .collect();
    let right_align = (0..result.columns.len())
        .map(|ci| detect_numeric_column(ci, result, &display_indices))
        .collect();

    DisplayView {
        visible_col_indices: Arc::new(visible_col_indices),
        display_indices: Arc::new(display_indices),
        default_col_widths: Arc::new(default_col_widths),
        right_align: Arc::new(right_align),
        truncated,
        cols_filtered,
        row_filtering,
        pre_filter_count,
    }
}

/// 渲染单次查询结果表格
///
/// 入口由 ResultPanel::render 调用，接收所有需要的主题色和上下文
#[allow(clippy::too_many_arguments)]
pub(super) fn render_table(
    panel: &ResultPanel,
    // 借用而非按值：避免每帧深拷贝整个结果集（大结果集卡顿主因）。
    // Arc 共享结果集；本帧只生成排序 / 过滤索引与少量列元数据。
    result: &Arc<QueryResult>,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    secondary_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted_bg: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let columns = &result.columns;
    let column_types = &result.column_types;
    let total_rows = result.rows.len();
    let affected = result.affected_rows;
    let elapsed = result.elapsed_ms;

    // 排序 + 列/行过滤统一走 compute_display_view（导出复用同一函数，保证所见即所导）
    let view = compute_display_view(panel, result, cx);
    let DisplayView {
        visible_col_indices,
        display_indices,
        default_col_widths,
        right_align,
        truncated,
        cols_filtered,
        row_filtering,
        pre_filter_count,
    } = view;
    let total_cols = columns.len();
    let visible_cols_count = visible_col_indices.len();
    let visible_count = display_indices.len();

    // DML/DDL：没有列，只显示 affected_rows
    if columns.is_empty() {
        return v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .text_color(fg)
                    .child(format!("✓ {affected} 行受影响")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("{elapsed} ms")),
            )
            .into_any_element();
    }

    // 注：0 行不再 early return；让 header + 空 body + 状态栏正常渲染，
    // 用户能看到列头与列类型，避免"查无结果"占位遮蔽元信息

    // 列宽 / 行号宽 / 总宽
    let col_widths: Vec<gpui::Pixels> = default_col_widths
        .iter()
        .enumerate()
        .map(|(ci, &default_width)| panel.col_width_override(ci).unwrap_or(default_width))
        .collect();
    let row_num_width = px((total_rows.to_string().len() as f32 * 9.0 + 16.0).clamp(40.0, 70.0));
    let checkbox_col_width = px(32.0);
    let total_content_width = visible_col_indices
        .iter()
        .map(|&ci| col_widths[ci])
        .fold(row_num_width + checkbox_col_width, |acc, w| acc + w);

    // 数据 cell 用 mono 字体（长 ID / 时间戳纵向对齐）；表头不用
    let mono_font = cx.theme().mono_font_family.clone();
    let current_sort = panel.sort_by();
    let header_cells: Vec<AnyElement> = visible_col_indices
        .iter()
        .map(|&ci| {
            render_header_cell(
                ci,
                columns,
                column_types,
                &col_widths,
                current_sort,
                fg,
                muted_fg,
                border,
                cx,
            )
        })
        .collect();

    let row_num_header = div()
        .w(row_num_width)
        .flex_none()
        .px_2()
        .border_r_1()
        .border_color(border)
        .into_any_element();

    let selected_rows_set = panel.selected_rows();
    let visible_row_indices = display_indices.clone();
    let all_selected = !visible_row_indices.is_empty()
        && visible_row_indices
            .iter()
            .all(|source_idx| selected_rows_set.contains(source_idx));
    let panel_entity = cx.entity();

    let checkbox_header = {
        let panel = panel_entity.clone();
        let visible_row_indices = visible_row_indices.clone();
        div()
            .w(checkbox_col_width)
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .items_center()
                    .justify_center()
                    .child(
                        Checkbox::new("rows-toggle-all")
                            .checked(all_selected)
                            .on_click(move |_: &bool, _, app| {
                                panel.update(app, |this, cx| {
                                    this.toggle_visible_rows(&visible_row_indices, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    };

    let header = h_flex()
        .w(total_content_width)
        .h(px(34.0))
        .flex_none()
        .items_center()
        .bg(secondary_bg)
        .border_b_1()
        .border_color(border)
        .child(checkbox_header)
        .child(row_num_header)
        .children(header_cells);

    // 不变数据装进 frame，Rc 共享给 closure 满足 'static + Fn
    let frame = Rc::new(TableRowFrame {
        result: result.clone(),
        display_indices,
        visible_col_indices: visible_col_indices.clone(),
        col_widths: col_widths.clone(),
        right_align,
        row_num_width,
        checkbox_col_width,
        total_content_width,
        mono_font,
        fg,
        muted_fg,
        border,
        muted_bg,
        accent,
    });

    let has_pending = panel.pending_insert().is_some();
    let dml_busy = panel.dml_busy();
    let row_count = frame.display_indices.len() + if has_pending { 1 } else { 0 };

    let frame_for_rows = frame.clone();
    let body = uniform_list(
        "result-rows",
        row_count,
        cx.processor(move |this, range: Range<usize>, _w, cx| {
            range
                .map(|i| {
                    if i < frame_for_rows.display_indices.len() {
                        render_data_row(this, &frame_for_rows, i, cx)
                    } else {
                        render_pending_row(this, &frame_for_rows, cx)
                    }
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(panel.uniform_scroll())
    .w(frame.total_content_width)
    .flex_1()
    // list 单 Y 滚，限制轴避免 wheel dx 被劫持
    .restrict_scroll_to_axis();

    // selected_cell 存源行下标（与 DML 一致）：直接索引原始 result.rows
    let selected_info: Option<String> = panel.selected_cell().and_then(|(ri, ci)| {
        let col_name = columns.get(ci)?.clone();
        let val = result.rows.get(ri)?.values.get(ci)?;
        let preview = val.display_preview(40);
        let hidden_note = if visible_row_indices.contains(&ri) {
            ""
        } else {
            "（当前隐藏）"
        };
        Some(format!(
            "· [{}, {}] = {}{hidden_note}",
            ri + 1,
            col_name,
            preview
        ))
    });
    let selected_count = selected_rows_set.len();
    let visible_selected = visible_row_indices
        .iter()
        .filter(|ri| selected_rows_set.contains(ri))
        .count();
    let hidden_selected = selected_count.saturating_sub(visible_selected);
    let selected_scope = (selected_count > 0).then(|| {
        if hidden_selected > 0 {
            format!("· 已选 {selected_count} 行，其中 {hidden_selected} 行当前隐藏")
        } else {
            format!("· 已选 {selected_count} 行")
        }
    });

    let status_bar = h_flex()
        .w_full()
        .flex_none()
        .items_center()
        .px_3()
        .py_1()
        .gap_2()
        .border_t_1()
        .border_color(border)
        .bg(secondary_bg)
        .text_xs()
        .text_color(muted_fg)
        .child(match (cols_filtered, row_filtering) {
            (true, true) => div().child(format!(
                "命中 {visible_cols_count} / {total_cols} 列 · {visible_count} / {pre_filter_count} 行"
            )),
            (true, false) => div().child(format!(
                "命中 {visible_cols_count} / {total_cols} 列 · {pre_filter_count} 行"
            )),
            (false, true) => {
                div().child(format!("命中 {visible_count} / {pre_filter_count} 行"))
            }
            (false, false) if truncated => div().child(format!(
                "显示 {MAX_ROWS_DISPLAY} / {total_rows} 行（已截断）"
            )),
            (false, false) => div().child(format!("{total_rows} 行")),
        })
        .child(div().child(format!("· 耗时 {elapsed} ms")))
        .when_some(selected_scope, |this, scope| this.child(div().child(scope)))
        .when_some(selected_info, |this, info| {
            this.child(div().overflow_hidden().text_ellipsis().child(info))
        })
        .when(has_pending, |this| {
            let panel_for_cancel = panel_entity.clone();
            let panel_for_submit = panel_entity.clone();
            this.child(div().flex_1())
                .child(
                    Button::new("insert-cancel-bar")
                        .ghost()
                        .small()
                        .label("取消")
                        .disabled(dml_busy)
                        .on_click(move |_, _, app| {
                            panel_for_cancel.update(app, |r, cx| r.cancel_insert(cx));
                        }),
                )
                .child(
                    Button::new("insert-submit-bar")
                        .primary()
                        .small()
                        .label(if dml_busy { "提交中…" } else { "提交" })
                        .disabled(dml_busy)
                        .on_click(move |_, _, app| {
                            panel_for_submit.update(app, |r, cx| r.submit_insert(cx));
                        }),
                )
        });

    // 外层布局：v_flex 主轴；水平滚动由外层 div 处理，垂直虚拟化由 list 处理
    // 关键：
    // 1) 外层 div 用 overflow_x_scroll（仅 X），list 用 track_scroll 管 Y；
    //    wheel 事件先到 list 消费 Y delta，剩余 X 冒泡给 div 消费 X delta —— 嵌套
    //    viewport 标准行为，触控板含 Y 噪声时 list 也会少量滚动 Y
    // 2) 外层 div 通过 panel.h_scroll() 关联 ScrollHandle，跨 render 保持水平位置；
    //    切表时由 set_state 调 set_offset 主动归位左侧
    // 3) 内层 v_flex 用 h_full 而非 size_full —— size_full 含 w_full 会重置 width
    v_flex()
        .size_full()
        .min_w_0()
        .child(
            div()
                .id("result-h-scroll")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_x_scroll()
                // 禁止外层 div 把 wheel dy 当 dx 用（div 是单 X 滚，否则 dy 会被劫持横向滚）
                .restrict_scroll_to_axis()
                .track_scroll(panel.h_scroll())
                .child(
                    v_flex()
                        .w(frame.total_content_width)
                        .h_full()
                        .child(header)
                        .child(body),
                ),
        )
        .child(status_bar)
        .into_any_element()
}

/// Header 单元格：列名（强）+ 类型副标（弱）+ 排序箭头（弱）+ 列宽拖拽 handle
mod cells;
mod helpers;

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
    fn display_view_cache_reuses_only_matching_revision_and_inputs() {
        let view = build_display_view(&sample_result(), None, "", "");
        let key = DisplayViewCacheKey {
            result_identity: 7,
            result_revision: 3,
            sort_by: None,
            column_filter: String::new(),
            row_filter_lower: String::new(),
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
}
