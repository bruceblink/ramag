//! 详情面板：选中条目的完整内容 + 元信息 + 操作按钮

use std::sync::Arc;

use chrono::Utc;
use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, img, prelude::*, px,
};
use gpui_component::{ActiveTheme, Sizable as _, button::ButtonVariants as _, h_flex, v_flex};
use ramag_domain::entities::{ClipItem, ClipKind, blacklist_matches};
use ramag_ui::platform::file_manager_reveal_label;

use super::ClipboardView;
use crate::views::helpers::relative_time;

const MAX_DETAIL_TEXT_BYTES: usize = 128 * 1024;
const MAX_DETAIL_FILE_ROWS: usize = 500;
const MAX_DETAIL_PATH_BYTES: usize = 4 * 1024;
const DETAIL_TEXT_NOTICE: &str = "\n\n[内容过大，仅展示前 128 KiB；复制和粘贴仍使用完整内容]";

impl ClipboardView {
    pub(super) fn render_detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let Some(item) = self.selected_item(cx) else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child("选择左侧条目查看详情")
                .into_any_element();
        };

        let header = self.detail_header(item.as_ref(), cx).into_any_element();
        let body = self.detail_body(item.clone(), cx);
        let actions = self.detail_actions(item, cx);

        v_flex()
            .size_full()
            .p(px(16.0))
            .gap(px(12.0))
            .child(header)
            .child(
                div()
                    .id("clip-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .children(actions)
            .into_any_element()
    }

    fn detail_header(&self, item: &ClipItem, cx: &Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let source = item
            .source
            .as_ref()
            .map(|s| format!("来源：{}", s.name))
            .unwrap_or_default();
        let meta = format!(
            "{} · {} · {} 字节",
            item.kind.label(),
            relative_time(item.last_used_at, Utc::now()),
            item.byte_size
        );
        v_flex()
            .gap(px(2.0))
            .child(div().text_sm().text_color(muted).child(meta))
            .when(!source.is_empty(), |this| {
                this.child(div().text_xs().text_color(muted).child(source))
            })
    }

    /// 只展示卡片行没有的上下文操作。
    fn detail_actions(
        &self,
        item: Arc<ClipItem>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let mut buttons = Vec::new();
        let contextual = match item.kind {
            ClipKind::Link if item.text.is_some() => {
                let item = item.clone();
                Some(
                    ramag_ui::clickable_button("detail-open")
                        .small()
                        .label("打开")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(url) = &item.text {
                                this.open_link(url.clone(), cx);
                            }
                        }))
                        .into_any_element(),
                )
            }
            ClipKind::Files => {
                let item = item.clone();
                Some(
                    ramag_ui::clickable_button("detail-reveal")
                        .small()
                        .label(file_manager_reveal_label())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.reveal_files(&item.files, cx);
                        }))
                        .into_any_element(),
                )
            }
            _ if item.rtf.is_some() => {
                let item_plain = item.clone();
                Some(
                    ramag_ui::clickable_button("detail-plain")
                        .small()
                        .label("复制文本")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.copy_plain(item_plain.clone(), cx);
                        }))
                        .into_any_element(),
                )
            }
            _ => None,
        };
        if let Some(button) = contextual {
            buttons.push(button);
        }

        if let Some(source) = &item.source
            && !self
                .settings
                .blacklist
                .iter()
                .any(|id| blacklist_matches(id, &source.bundle_id))
        {
            let source_id = source.bundle_id.clone();
            buttons.push(
                ramag_ui::clickable_button("detail-blacklist-source")
                    .danger()
                    .small()
                    .label("排除")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.blacklist_source(source_id.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }

        if buttons.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .items_center()
                .gap(px(8.0))
                .children(buttons)
                .into_any_element(),
        )
    }

    fn detail_body(&mut self, item: Arc<ClipItem>, cx: &mut Context<Self>) -> gpui::AnyElement {
        match item.kind {
            ClipKind::Image => match self.image_for(item.clone(), false, cx) {
                Some(image) => img(image).max_w_full().into_any_element(),
                // 失败明示（媒体缺失 / 损坏 / 尺寸过大），不再永久显示假「加载中」
                None if self.image_failed(item.as_ref(), false) => div()
                    .text_sm()
                    .child("图片无法加载（文件可能缺失、损坏或尺寸过大）")
                    .into_any_element(),
                None => div().child("加载中…").into_any_element(),
            },
            ClipKind::Files => {
                let mut body = v_flex().gap(px(4.0)).children(
                    item.files.iter().take(MAX_DETAIL_FILE_ROWS).map(|path| {
                        div()
                            .text_sm()
                            .child(bounded_path_text(path))
                            .into_any_element()
                    }),
                );
                if item.files.len() > MAX_DETAIL_FILE_ROWS {
                    body = body.child(
                        div().text_xs().child(format!(
                            "文件较多，仅展示前 {MAX_DETAIL_FILE_ROWS} / {} 个；复制和粘贴仍使用完整列表",
                            item.files.len()
                        )),
                    );
                }
                body.into_any_element()
            }
            _ => {
                let display = self.detail_text(item.as_ref());
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(display)
                    .into_any_element()
            }
        }
    }

    fn detail_text(&mut self, item: &ClipItem) -> SharedString {
        if let Some((cached_id, text)) = &self.detail_text_cache
            && cached_id == &item.id
        {
            return text.clone();
        }
        let text = SharedString::from(bounded_detail_text(
            item.text.as_deref().unwrap_or_default(),
        ));
        self.detail_text_cache = Some((item.id.clone(), text.clone()));
        text
    }
}

fn bounded_detail_text(text: &str) -> String {
    if text.len() <= MAX_DETAIL_TEXT_BYTES {
        return text.to_string();
    }
    let content_limit = MAX_DETAIL_TEXT_BYTES.saturating_sub(DETAIL_TEXT_NOTICE.len());
    let mut end = content_limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut display = String::with_capacity(MAX_DETAIL_TEXT_BYTES);
    display.push_str(&text[..end]);
    display.push_str(DETAIL_TEXT_NOTICE);
    display
}

fn bounded_path_text(path: &str) -> String {
    if path.len() <= MAX_DETAIL_PATH_BYTES {
        return path.to_string();
    }
    let mut end = MAX_DETAIL_PATH_BYTES.saturating_sub('…'.len_utf8());
    while end > 0 && !path.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &path[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_text_is_bounded_without_splitting_unicode() {
        let text = "你".repeat(MAX_DETAIL_TEXT_BYTES);
        let display = bounded_detail_text(&text);

        assert!(display.len() <= MAX_DETAIL_TEXT_BYTES);
        assert!(display.contains("复制和粘贴仍使用完整内容"));
    }

    #[test]
    fn detail_path_is_bounded_without_splitting_unicode() {
        let path = "文".repeat(MAX_DETAIL_PATH_BYTES);
        let display = bounded_path_text(&path);

        assert!(display.len() <= MAX_DETAIL_PATH_BYTES);
        assert!(display.ends_with('…'));
    }
}
