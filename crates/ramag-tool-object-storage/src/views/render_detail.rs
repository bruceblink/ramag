use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, StyledExt as _, button::ButtonVariants as _, h_flex,
    v_flex,
};
use ramag_domain::entities::{ObjectMetadata, format_bytes};

use super::model::ObjectStorageView;

impl ObjectStorageView {
    pub(super) fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let mono = theme.mono_font_family.clone();
        let metadata = self.detail_metadata.clone();
        v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .id("object-detail-header")
                    .debug_selector(|| "object-detail-header".into())
                    .w_full()
                    .h(px(44.0))
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(10.0))
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_semibold()
                            .child("对象详情"),
                    )
                    .child(
                        ramag_ui::clickable_button("object-detail-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("关闭")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_detail = false;
                                this.persist_workspace(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("object-detail-scroll")
                    .debug_selector(|| "object-detail-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap(px(14.0))
                            .p(px(12.0))
                            .when_some(metadata, |body, metadata| {
                                body.child(metadata_section(
                                    metadata,
                                    border,
                                    muted,
                                    mono.clone(),
                                    cx,
                                ))
                            })
                            .when(self.detail_metadata.is_none(), |body| {
                                body.child(
                                    div()
                                        .id("object-detail-message")
                                        .w_full()
                                        .h(px(180.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .px(px(20.0))
                                        .text_center()
                                        .text_sm()
                                        .text_color(muted)
                                        .child(self.detail_message.clone()),
                                )
                            }),
                    ),
            )
    }
}

fn metadata_section(
    metadata: ObjectMetadata,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    mono: SharedString,
    cx: &gpui::App,
) -> AnyElement {
    let modified = metadata
        .last_modified
        .map(|value| value.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "—".into());
    let size = format!(
        "{}（{} B）",
        format_bytes(metadata.size),
        grouped_integer(metadata.size)
    );
    let mut card = v_flex()
        .w_full()
        .min_w_0()
        .gap(px(8.0))
        .child(section_heading("基本信息", muted))
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(px(7.0))
                .p(px(10.0))
                .rounded_md()
                .border_1()
                .border_color(border)
                .bg(cx.theme().secondary)
                .child(metadata_row(
                    "对象键",
                    metadata.key,
                    true,
                    muted,
                    mono.clone(),
                ))
                .child(metadata_row("大小", size, false, muted, mono.clone()))
                .child(metadata_row(
                    "内容类型",
                    metadata.content_type.unwrap_or_else(|| "未知".into()),
                    false,
                    muted,
                    mono.clone(),
                ))
                .child(metadata_row(
                    "存储类型",
                    metadata.storage_class.unwrap_or_else(|| "未知".into()),
                    false,
                    muted,
                    mono.clone(),
                ))
                .child(metadata_row(
                    "最后修改",
                    modified,
                    false,
                    muted,
                    mono.clone(),
                ))
                .child(metadata_row(
                    "ETag",
                    metadata.etag.unwrap_or_else(|| "—".into()),
                    true,
                    muted,
                    mono.clone(),
                ))
                .child(metadata_row(
                    "版本",
                    metadata.version.unwrap_or_else(|| "—".into()),
                    true,
                    muted,
                    mono.clone(),
                )),
        );
    if metadata.user_metadata.is_empty() {
        card = card.child(div().text_xs().text_color(muted).child("无自定义元数据"));
    } else {
        card = card.child(section_heading("自定义元数据", muted)).child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(px(7.0))
                .p(px(10.0))
                .rounded_md()
                .border_1()
                .border_color(border)
                .children(
                    metadata
                        .user_metadata
                        .into_iter()
                        .map(|(key, value)| metadata_row(key, value, true, muted, mono.clone())),
                ),
        );
    }
    card.into_any_element()
}

fn metadata_row(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    mono: bool,
    muted: gpui::Hsla,
    mono_font: SharedString,
) -> AnyElement {
    let label: SharedString = label.into();
    let value: SharedString = value.into();
    let value_for_copy = value.clone();
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap(px(8.0))
        .text_xs()
        .child(
            div()
                .w(px(64.0))
                .flex_none()
                .text_color(muted)
                .child(label.clone()),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "object-metadata-value-{}",
                    label.as_str()
                )))
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .when(mono, |value| value.font_family(mono_font))
                .on_click(move |event: &ClickEvent, _, app| {
                    if ramag_ui::is_primary_modifier_double_click(event) {
                        ramag_ui::copy_text(value_for_copy.to_string(), app);
                    }
                })
                .child(value),
        )
        .into_any_element()
}

fn section_heading(label: &'static str, muted: gpui::Hsla) -> impl IntoElement {
    div()
        .text_xs()
        .font_semibold()
        .text_color(muted)
        .child(label)
}

fn grouped_integer(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::grouped_integer;

    #[test]
    fn exact_byte_count_is_grouped_for_readability() {
        assert_eq!(grouped_integer(0), "0");
        assert_eq!(grouped_integer(17_915_233), "17,915,233");
    }
}
