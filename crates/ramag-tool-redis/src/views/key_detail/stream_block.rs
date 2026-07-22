//! Stream 块：uniform_list 行级虚拟化。条目「头行 + 各字段行」扁平成等高行序列，
//! 因 entry 字段数可变（不等高），无法直接按 entry 虚拟化，故先扁平再喂 uniform_list

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled,
    UniformListScrollHandle, div, prelude::*, px, uniform_list,
};
use gpui_component::{Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex};
use ramag_domain::entities::{MAX_REDIS_COMMAND_ARG_BYTES, RedisValue, StreamEntry};

use super::{KeyDetailEvent, KeyDetailPanel};
use crate::views::inline_text_preview;

/// 行高固定 28px：uniform_list 行级虚拟化要求等高（头行 / 字段行同高）
const ROW_H: f32 = 28.0;

/// 扁平后的行：条目头（ID + 删除按钮）或单个字段（k=v）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamRow {
    Header { entry: usize },
    Field { entry: usize, field: usize },
}

pub(super) fn render_stream_block(
    panel: &mut Context<KeyDetailPanel>,
    key: String,
    entries: &[StreamEntry],
    scroll: &UniformListScrollHandle,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement + use<> {
    // 只缓存索引，不复制字段正文；大 Stream 重渲染时避免再次深拷贝整份数据。
    let rows = Rc::new(flatten_stream_rows(entries));
    let count = rows.len();
    let rows_for_closure = rows.clone();

    div()
        .debug_selector(|| "redis-stream-block".into())
        .flex_col()
        .size_full()
        .min_h_0()
        .border_1()
        .border_color(border)
        .rounded(px(4.0))
        .child(
            uniform_list(
                "stream-rows",
                count,
                panel.processor(move |this, range: Range<usize>, _w, cx| {
                    let Some(RedisValue::Stream(entries)) = &this.value else {
                        return Vec::new();
                    };
                    let read_only = this.is_read_only();
                    range
                        .filter_map(|i| {
                            let row = rows_for_closure.get(i)?;
                            stream_row(row, entries, &key, read_only, fg, muted_fg, border, cx)
                        })
                        .collect()
                }),
            )
            .track_scroll(scroll)
            .size_full(),
        )
}

fn flatten_stream_rows(entries: &[StreamEntry]) -> Vec<StreamRow> {
    let row_count = entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(1).saturating_add(entry.fields.len())
    });
    let mut rows = Vec::with_capacity(row_count);
    for (entry_index, entry) in entries.iter().enumerate() {
        rows.push(StreamRow::Header { entry: entry_index });
        rows.extend((0..entry.fields.len()).map(|field| StreamRow::Field {
            entry: entry_index,
            field,
        }));
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn stream_row(
    row: &StreamRow,
    entries: &[StreamEntry],
    key: &str,
    read_only: bool,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
    cx: &mut Context<KeyDetailPanel>,
) -> Option<AnyElement> {
    match row {
        StreamRow::Header { entry } => {
            let item = entries.get(*entry)?;
            let id_for_del =
                (item.id.len() <= MAX_REDIS_COMMAND_ARG_BYTES).then(|| item.id.clone());
            let key_for_del = key.to_string();
            let del_id = SharedString::from(format!("stream-del-{entry}"));
            Some(
                h_flex()
                    .h(px(ROW_H))
                    .flex_none()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    // 顶边线分隔相邻条目
                    .border_t_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(inline_text_preview(&item.id, 128)),
                    )
                    .child(
                        ramag_ui::clickable_button(del_id)
                            .ghost()
                            .xsmall()
                            .icon(ramag_ui::icons::trash())
                            .disabled(read_only || id_for_del.is_none())
                            .when(read_only || id_for_del.is_none(), |button| {
                                button.tooltip(if read_only { "只读" } else { "ID 过大" })
                            })
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                if let Some(id) = id_for_del.clone() {
                                    cx.emit(KeyDetailEvent::RequestDeleteStreamEntry(
                                        key_for_del.clone(),
                                        id,
                                    ));
                                }
                            })),
                    )
                    .into_any_element(),
            )
        }
        StreamRow::Field { entry, field } => {
            let (key, value) = entries.get(*entry)?.fields.get(*field)?;
            Some(
                h_flex()
                    .h(px(ROW_H))
                    .flex_none()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .pl(px(20.0))
                    .pr(px(8.0))
                    .child(
                        div()
                            .w(px(140.0))
                            .text_xs()
                            .text_color(muted_fg)
                            .flex_none()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(inline_text_preview(key, 128)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(fg)
                            .font_family("monospace")
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(inline_text_preview(value, 256)),
                    )
                    .into_any_element(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamRow, flatten_stream_rows};
    use ramag_domain::entities::StreamEntry;

    #[test]
    fn flattened_stream_rows_store_only_payload_indices() {
        let entries = vec![
            StreamEntry {
                id: "1-0".into(),
                fields: vec![("a".into(), "1".into()), ("b".into(), "2".into())],
            },
            StreamEntry {
                id: "2-0".into(),
                fields: vec![],
            },
        ];

        assert_eq!(
            flatten_stream_rows(&entries),
            vec![
                StreamRow::Header { entry: 0 },
                StreamRow::Field { entry: 0, field: 0 },
                StreamRow::Field { entry: 0, field: 1 },
                StreamRow::Header { entry: 1 },
            ]
        );
    }
}
