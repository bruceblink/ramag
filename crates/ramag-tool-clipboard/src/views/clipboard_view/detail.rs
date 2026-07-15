//! 详情面板：选中条目的完整内容 + 元信息 + 操作按钮

use std::sync::Arc;

use chrono::Utc;
use gpui::{ClickEvent, Context, IntoElement, ParentElement, Styled, div, img, prelude::*, px};
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use ramag_domain::entities::{ClipItem, ClipKind, blacklist_matches};
use ramag_ui::platform::file_manager_reveal_label;

use super::ClipboardView;
use crate::views::helpers::relative_time;

impl ClipboardView {
    pub(super) fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

    /// 详情底部只保留卡片行没有的上下文动作（浏览器打开 / 文件管理器显示 / 纯文本复制）。
    /// 复制 / 删除已由卡片行图标按钮覆盖，不在详情重复。无适用动作时返回 None
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
                    Button::new("detail-open")
                        .small()
                        .label("在浏览器打开")
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
                    Button::new("detail-reveal")
                        .small()
                        .label(file_manager_reveal_label())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.reveal_files(item.files.clone(), cx);
                        }))
                        .into_any_element(),
                )
            }
            _ if item.rtf.is_some() => {
                let item_plain = item.clone();
                Some(
                    Button::new("detail-plain")
                        .small()
                        .label("复制为纯文本")
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
                Button::new("detail-blacklist-source")
                    .danger()
                    .small()
                    .label("不再记录此应用")
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

    /// 详情主体：图片显示大图（解密原图），文件列路径，文本显示全文
    fn detail_body(&self, item: Arc<ClipItem>, cx: &mut Context<Self>) -> gpui::AnyElement {
        match item.kind {
            ClipKind::Image => match self.image_for(item.clone(), false, cx) {
                Some(image) => img(image).max_w_full().into_any_element(),
                // 失败明示（媒体文件缺失 / 解密失败），不再永久显示假「加载中」
                None if self.image_failed(item.as_ref(), false) => div()
                    .text_sm()
                    .child("图片无法解密或已缺失（媒体文件可能被清理）")
                    .into_any_element(),
                None => div().child("加载中…").into_any_element(),
            },
            ClipKind::Files => v_flex()
                .gap(px(4.0))
                .children(
                    item.files
                        .iter()
                        .map(|f| div().text_sm().child(f.clone()).into_any_element()),
                )
                .into_any_element(),
            _ => div()
                .text_sm()
                .whitespace_normal()
                .child(item.text.clone().unwrap_or_default())
                .into_any_element(),
        }
    }
}
