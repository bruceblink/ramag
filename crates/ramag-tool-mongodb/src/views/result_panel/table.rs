use std::ops::Range;
use std::sync::Arc;

use gpui::{
    Context, Hsla, InteractiveElement as _, IntoElement, ParentElement, ScrollWheelEvent,
    SharedString, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use ramag_ui::RestrictScrollToAxisExt as _;

use super::flatten::{Column, FlatTable};
use super::{ResultPanel, SortDir};

pub(super) const CELL_WIDTH: f32 = 200.0;
pub(super) const ROW_HEIGHT: f32 = 32.0;
const HEADER_HEIGHT: f32 = 34.0;
pub(super) const CELL_PREVIEW_MAX: usize = 80;
const CHECKBOX_WIDTH: f32 = 32.0;

/// 构建 MongoDB 结果表：保持行虚拟化，并为扁平化后的宽文档提供横向浏览入口。
pub(super) fn render(
    panel: &mut ResultPanel,
    table: Arc<FlatTable>,
    col_indices: Option<Vec<usize>>,
    row_indices: Arc<Vec<usize>>,
    allow_edit: bool,
    drill_docs: Option<Arc<Vec<serde_json::Value>>>,
    cx: &mut Context<ResultPanel>,
) -> impl IntoElement {
    let border = cx.theme().border;
    let fg = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let secondary_bg = cx.theme().secondary;
    let mono_font = cx.theme().mono_font_family.clone();

    let drill_view = drill_docs.is_some();
    let visible_cols: Vec<usize> =
        col_indices.unwrap_or_else(|| (0..table.columns.len()).collect());
    let visible_rows = row_indices;

    let row_num_width =
        px((table.rows.len().to_string().len() as f32 * 9.0 + 16.0).clamp(40.0, 70.0));

    let header_checkbox = if drill_view {
        checkbox_placeholder(border)
    } else {
        let all_data_idx = visible_rows.clone();
        let all_selected = panel.all_visible_rows_selected(&all_data_idx);
        let entity_for_all = cx.entity().clone();
        div()
            .w(px(CHECKBOX_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .border_r_1()
            .border_color(border)
            .child(
                ramag_ui::clickable_checkbox("mongo-cb-all")
                    .checked(all_selected)
                    .on_click(move |_: &bool, _, app| {
                        entity_for_all.update(app, |this, cx| this.toggle_all(&all_data_idx, cx))
                    }),
            )
            .into_any_element()
    };

    let header_row = render_header(
        header_checkbox,
        row_num_width,
        &table.columns,
        &visible_cols,
        panel.sort_by.clone(),
        fg,
        muted,
        border,
        secondary_bg,
        cx,
    );
    let total_width = px(CHECKBOX_WIDTH + CELL_WIDTH * visible_cols.len() as f32) + row_num_width;

    let table_for_list = table.clone();
    let cols_for_list = visible_cols.clone();
    let rows_for_list = visible_rows;
    let drill_docs_for_list = drill_docs.clone();
    let body = uniform_list(
        "mongo-result-rows",
        rows_for_list.len(),
        cx.processor(move |panel, range: Range<usize>, _w, cx| {
            let theme = cx.theme();
            let fg = theme.foreground;
            let muted = theme.muted_foreground;
            let nested_fg = theme.blue;
            let border = theme.border;
            let muted_bg = theme.muted;
            let mono = mono_font.clone();
            range
                .map(|i| {
                    let row_idx = rows_for_list[i];
                    let row = &table_for_list.rows[row_idx];
                    let checkbox = if drill_view {
                        checkbox_placeholder(border)
                    } else {
                        let selected = panel.is_row_selected(row_idx);
                        let entity_for_row = cx.entity().clone();
                        div()
                            .w(px(CHECKBOX_WIDTH))
                            .flex_none()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_r_1()
                            .border_color(border)
                            .child(
                                ramag_ui::clickable_checkbox(SharedString::from(format!(
                                    "mongo-cb-{i}"
                                )))
                                .checked(selected)
                                .on_click(
                                    move |_: &bool, _, app| {
                                        entity_for_row
                                            .update(app, |this, cx| this.toggle_row(row_idx, cx))
                                    },
                                ),
                            )
                            .into_any_element()
                    };
                    let drill_doc = drill_docs_for_list
                        .as_ref()
                        .and_then(|docs| docs.get(row_idx));
                    super::row::render_row(
                        checkbox,
                        row_num_width,
                        i,
                        row_idx,
                        row,
                        &cols_for_list,
                        &table_for_list.columns,
                        fg,
                        muted,
                        nested_fg,
                        border,
                        muted_bg,
                        mono.clone(),
                        allow_edit,
                        drill_doc,
                        cx,
                    )
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(&panel.uniform_scroll)
    .w(total_width)
    .flex_1()
    .restrict_scroll_to_axis();

    // 外层横向滚动，内层纵向虚拟化；滚动条使用固定底部布局行，避免被结果内容覆盖。
    let table_view = div()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            div()
                .id("mongo-table-h-scroll")
                .debug_selector(|| "mongo-table-h-scroll".into())
                .size_full()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&panel.h_scroll)
                .child(
                    v_flex()
                        .w(total_width)
                        .h_full()
                        .child(header_row.flex_none())
                        .child(body),
                ),
        )
        .child(
            div()
                .id("mongo-table-scroll-input")
                .absolute()
                .inset_0()
                .on_scroll_wheel(cx.listener(ResultPanel::on_table_scroll)),
        )
        .child(
            div()
                .id("mongo-table-v-scrollbar")
                .debug_selector(|| "mongo-table-v-scrollbar".into())
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(16.0))
                .bg(cx.theme().scrollbar)
                .child(
                    Scrollbar::vertical(&panel.uniform_scroll)
                        .id("mongo-table-v-scrollbar-control")
                        .scrollbar_show(ScrollbarShow::Always),
                ),
        );

    let table_container = v_flex()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(table_view)
        .child(
            div()
                .id("mongo-table-h-scrollbar")
                .debug_selector(|| "mongo-table-h-scrollbar".into())
                .flex_none()
                .w_full()
                .h(px(16.0))
                .relative()
                .bg(cx.theme().scrollbar)
                .child(
                    Scrollbar::horizontal(&panel.h_scroll)
                        .id("mongo-table-h-scrollbar-control")
                        .scroll_size(gpui::size(total_width, px(16.0)))
                        .scrollbar_show(ScrollbarShow::Always),
                ),
        );

    v_flex().size_full().min_w_0().child(table_container)
}

impl ResultPanel {
    fn on_table_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let horizontal = self.h_scroll.clone();
        let vertical = self.uniform_scroll.0.borrow().base_handle.clone();
        ramag_ui::handle_axis_scroll(
            &mut self.scroll_gesture,
            event,
            window,
            &horizontal,
            &vertical,
            cx,
        );
    }
}

fn checkbox_placeholder(border: Hsla) -> gpui::AnyElement {
    div()
        .w(px(CHECKBOX_WIDTH))
        .flex_none()
        .h_full()
        .border_r_1()
        .border_color(border)
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_header(
    checkbox: gpui::AnyElement,
    row_num_width: gpui::Pixels,
    columns: &[Column],
    visible_cols: &[usize],
    current_sort: Option<(String, SortDir)>,
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
    bg: Hsla,
    cx: &mut Context<ResultPanel>,
) -> gpui::Div {
    let row_num_cell = div()
        .w(row_num_width)
        .flex_none()
        .h_full()
        .border_r_1()
        .border_color(border);

    let mut row = h_flex()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .items_center()
        .bg(bg)
        .border_b_1()
        .border_color(border)
        .child(checkbox)
        .child(row_num_cell);
    for &ci in visible_cols {
        let col = &columns[ci];
        let path = col.path.clone();
        let kind = col.kind;
        let arrow: Option<&'static str> = match &current_sort {
            Some((p, SortDir::Asc)) if *p == path => Some("▲"),
            Some((p, SortDir::Desc)) if *p == path => Some("▼"),
            _ => None,
        };
        let path_for_click = path.clone();
        row = row.child(
            h_flex()
                .id(SharedString::from(format!("mongo-hdr-{ci}")))
                .w(px(CELL_WIDTH))
                .flex_none()
                .h_full()
                .px_3()
                .gap_1p5()
                .items_center()
                .border_r_1()
                .border_color(border)
                .text_xs()
                .overflow_hidden()
                .cursor_pointer()
                .on_click(cx.listener(move |panel, _: &gpui::ClickEvent, _, cx| {
                    panel.toggle_sort(path_for_click.clone(), cx)
                }))
                .child(
                    div()
                        .min_w_0()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(sanitize_inline(&path))),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(muted)
                        .whitespace_nowrap()
                        .child(SharedString::from(kind)),
                )
                .when_some(arrow, |this, a| {
                    this.child(div().flex_none().text_color(muted).child(a))
                }),
        );
    }
    row
}

/// 空值在前，数值列按数值排序，其余按文本排序。
pub(super) fn compare_cells(a: &str, b: &str, numeric: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    if numeric {
        if let (Some(x), Some(y)) = (DecimalKey::parse(a), DecimalKey::parse(b)) {
            return x.cmp(&y);
        }
        if let (Ok(x), Ok(y)) = (a.parse::<f64>(), b.parse::<f64>()) {
            return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
        }
    }
    a.cmp(b)
}

/// 可比较的有限十进制数；保留任意位数，避免 Decimal128 / Int64 经 f64 丢精度。
#[derive(Clone, Debug, PartialEq, Eq)]
struct DecimalKey {
    negative: bool,
    /// 首个有效数字相对小数点的位置；越大绝对值越大。
    magnitude: i64,
    digits: Vec<u8>,
}

impl DecimalKey {
    fn parse(text: &str) -> Option<Self> {
        let (negative, unsigned) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
            None => (unsigned, 0),
        };
        let mut digits = Vec::with_capacity(mantissa.len());
        let mut decimal_position = None;
        for byte in mantissa.bytes() {
            match byte {
                b'0'..=b'9' => digits.push(byte),
                b'.' if decimal_position.is_none() => {
                    decimal_position = Some(i64::try_from(digits.len()).ok()?);
                }
                _ => return None,
            }
        }
        if digits.is_empty() {
            return None;
        }
        let decimal_position = decimal_position
            .unwrap_or_else(|| i64::try_from(digits.len()).unwrap_or(i64::MAX))
            .checked_add(exponent)?;
        let leading = digits.iter().position(|digit| *digit != b'0');
        let Some(leading) = leading else {
            return Some(Self {
                negative: false,
                magnitude: 0,
                digits: Vec::new(),
            });
        };
        let magnitude = decimal_position.checked_sub(i64::try_from(leading).ok()?)?;
        digits.drain(..leading);
        while digits.last() == Some(&b'0') {
            digits.pop();
        }
        Some(Self {
            negative,
            magnitude,
            digits,
        })
    }

    fn absolute_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.magnitude.cmp(&other.magnitude).then_with(|| {
            let len = self.digits.len().max(other.digits.len());
            (0..len)
                .map(|index| {
                    self.digits
                        .get(index)
                        .copied()
                        .unwrap_or(b'0')
                        .cmp(&other.digits.get(index).copied().unwrap_or(b'0'))
                })
                .find(|ordering| !ordering.is_eq())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

impl Ord for DecimalKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.digits.is_empty() && other.digits.is_empty() {
            return std::cmp::Ordering::Equal;
        }
        match (self.negative, other.negative) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => self.absolute_cmp(other).reverse(),
            (false, false) => self.absolute_cmp(other),
        }
    }
}

impl PartialOrd for DecimalKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 数值列预解析一次，避免比较阶段重复解析。
pub(super) fn sort_row_indices(
    table: &FlatTable,
    column_index: usize,
    numeric: bool,
    direction: SortDir,
    indices: &mut [usize],
) {
    let numeric_values = numeric.then(|| {
        table
            .rows
            .iter()
            .map(|row| DecimalKey::parse(&row[column_index].text))
            .collect::<Vec<_>>()
    });
    indices.sort_by(|&left, &right| {
        let left_text = &table.rows[left][column_index].text;
        let right_text = &table.rows[right][column_index].text;
        let ordering = numeric_values
            .as_ref()
            .and_then(|values| values[left].as_ref().zip(values[right].as_ref()))
            .map(|(left, right)| left.cmp(right))
            .unwrap_or_else(|| compare_cells(left_text, right_text, numeric));
        if matches!(direction, SortDir::Desc) {
            ordering.reverse()
        } else {
            ordering
        }
    });
}

pub(super) fn truncate(s: &str, max_len: usize) -> String {
    let mut chars = s.chars();
    let mut preview: String = chars.by_ref().take(max_len).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

/// GPUI 单行文本不接受换行，仅清洗显示副本。
pub(super) fn sanitize_inline(s: &str) -> String {
    if s.contains(['\n', '\r']) {
        s.replace(['\n', '\r'], " ")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_string() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_adds_ellipsis_for_long() {
        let s = truncate("abcdefghijklmnop", 5);
        assert_eq!(s, "abcde…");
    }

    #[test]
    fn sanitize_inline_strips_newlines() {
        assert_eq!(sanitize_inline("a\nb"), "a b");
        assert_eq!(sanitize_inline("a\rb"), "a b");
        let s = sanitize_inline("x\ny\r\nz");
        assert!(!s.contains('\n') && !s.contains('\r'));
    }

    #[test]
    fn sanitize_inline_keeps_plain_text() {
        assert_eq!(sanitize_inline("plain text"), "plain text");
    }

    #[test]
    fn compare_cells_numeric_vs_text() {
        use std::cmp::Ordering;
        assert_eq!(compare_cells("9", "10", true), Ordering::Less);
        assert_eq!(compare_cells("9", "10", false), Ordering::Greater);
        assert_eq!(compare_cells("", "x", false), Ordering::Less);
        assert_eq!(compare_cells("x", "", false), Ordering::Greater);
        assert_eq!(
            compare_cells("9007199254740992", "9007199254740993", true),
            Ordering::Less
        );
        assert_eq!(
            compare_cells("1.0000000000000000001", "1.0000000000000000002", true),
            Ordering::Less
        );
        assert_eq!(compare_cells("-1.2e3", "-1199", true), Ordering::Less);
    }

    #[test]
    fn numeric_row_sort_keeps_fallback_semantics() {
        use super::super::cell::Cell;

        let table = FlatTable {
            columns: vec![Column {
                path: "n".into(),
                kind: "int",
            }],
            total_columns: 1,
            rows: ["10", "9", "x"]
                .into_iter()
                .map(|text| {
                    vec![Cell {
                        text: text.into(),
                        kind: "int",
                    }]
                })
                .collect(),
        };
        let mut indices = vec![0, 1, 2];

        sort_row_indices(&table, 0, true, SortDir::Asc, &mut indices);

        assert_eq!(indices, vec![1, 0, 2]);
    }
}
