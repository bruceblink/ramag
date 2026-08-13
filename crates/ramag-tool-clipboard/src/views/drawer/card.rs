use std::sync::Arc;

use chrono::Utc;
use gpui::{
    ClickEvent, Context, Hsla, IntoElement, ParentElement, SharedString, Styled, div, img,
    prelude::*, px,
};
use gpui::{Image, ImageFormat, ImageSource};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use ramag_domain::entities::{ClipItem, ClipKind};

use super::{CARD_WIDTH, ClipboardDrawer};
use crate::views::helpers::relative_time;

impl ClipboardDrawer {
    pub(super) fn render_card(
        &self,
        ix: usize,
        item: Arc<ClipItem>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // 释放主题借用，避免与图片加载的可变借用冲突。
        let border = cx.theme().border;
        let secondary = cx.theme().secondary;
        let muted = cx.theme().muted_foreground;
        let selected = ix == self.selected;
        let header_bg = kind_color(item.kind);
        let blue = gpui::hsla(212.0 / 360.0, 1.0, 0.52, 1.0);
        let thumb = if matches!(item.kind, ClipKind::Image) {
            self.thumb_image(item.clone(), cx)
        } else {
            None
        };
        let header = self
            .card_header(item.as_ref(), header_bg, cx)
            .into_any_element();
        let body = card_body(item.as_ref(), thumb);

        v_flex()
            .id(SharedString::from(format!("drawer-card-{}", item.id)))
            .w(px(CARD_WIDTH))
            .h_full()
            .flex_none()
            .rounded(px(10.0))
            .overflow_hidden()
            .border_2()
            .border_color(if selected { blue } else { border })
            .bg(secondary)
            .cursor_pointer()
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                if ev.click_count() >= 2 {
                    this.paste(ix, window, cx);
                } else {
                    this.selected = ix;
                    cx.notify();
                }
            }))
            .child(header)
            .child(body)
            .child(card_footer(item.as_ref(), muted))
    }

    fn card_header(&self, item: &ClipItem, bg: Hsla, cx: &Context<Self>) -> impl IntoElement {
        let mut sub = gpui::white();
        sub.a = 0.75;
        let icon = self.source_icon(item, cx);

        h_flex()
            .w_full()
            .flex_none()
            .h(px(56.0))
            .px(px(12.0))
            .py(px(8.0))
            .bg(bg)
            .items_start()
            .justify_between()
            .child(
                v_flex()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(gpui::white())
                            .child(item.kind.label_en()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(sub)
                            .child(relative_time(item.last_used_at, Utc::now())),
                    ),
            )
            .children(icon)
    }

    fn source_icon(&self, item: &ClipItem, _cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let bundle = item.source.as_ref().map(|s| s.bundle_id.as_str())?;
        let cache_key = format!("app-icon:{bundle}");
        let image = match self.img_cache.peek(&cache_key) {
            Some(image) => image,
            None => {
                if self.img_cache.is_failed(&cache_key) {
                    return None;
                }
                let png = self.service().app_icon(bundle)?;
                let Some(retained_bytes) =
                    crate::views::image_cache::png_retained_bytes(png.as_ref())
                else {
                    self.img_cache.fail(&cache_key);
                    return None;
                };
                let image = Arc::new(Image::from_bytes(ImageFormat::Png, png.as_ref().clone()));
                self.img_cache
                    .insert(cache_key, image.clone(), retained_bytes);
                image
            }
        };
        Some(
            img(ImageSource::Image(image))
                .size(px(34.0))
                .rounded(px(7.0))
                .into_any_element(),
        )
    }
}

fn card_body(item: &ClipItem, thumb: Option<Arc<Image>>) -> gpui::AnyElement {
    match item.kind {
        ClipKind::Image => div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(
                gpui::svg()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .path("icons/checker.svg"),
            )
            .when_some(thumb, |this, image| {
                this.child(
                    img(image)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(gpui::ObjectFit::Contain),
                )
            })
            .into_any_element(),
        _ => div()
            .flex_1()
            .min_h_0()
            .w_full()
            .p(px(12.0))
            .text_sm()
            .overflow_hidden()
            .child(item.preview.clone())
            .into_any_element(),
    }
}

fn card_footer(item: &ClipItem, muted: Hsla) -> impl IntoElement {
    let label = match item.kind {
        ClipKind::Image => item
            .image_dims
            .map(|(w, h)| format!("{w} × {h}"))
            .unwrap_or_default(),
        ClipKind::Files => format!("{} 个文件", item.files.len()),
        _ => {
            let bytes = item.text.as_ref().map_or(0, String::len);
            format!("{bytes} 字节")
        }
    };
    div()
        .w_full()
        .flex_none()
        .h(px(32.0))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(muted)
        .child(label)
}

fn kind_color(kind: ClipKind) -> Hsla {
    use gpui::hsla;
    match kind {
        ClipKind::Image => hsla(145.0 / 360.0, 0.62, 0.45, 1.0),
        ClipKind::Text => hsla(0.0, 0.0, 0.17, 1.0),
        ClipKind::Link => hsla(212.0 / 360.0, 0.7, 0.5, 1.0),
        ClipKind::Color => hsla(280.0 / 360.0, 0.5, 0.5, 1.0),
        ClipKind::Files => hsla(32.0 / 360.0, 0.8, 0.5, 1.0),
    }
}
