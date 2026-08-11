use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    img, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, StyledExt as _, button::ButtonVariants as _,
    h_flex, input::Input, v_flex,
};
use ramag_domain::entities::CloudProvider;

use super::AccountFormPanel;

impl AccountFormPanel {
    fn render_provider_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let accent = theme.accent;
        let border = theme.border;
        let secondary = theme.secondary;
        let mut accent_tint = accent;
        accent_tint.a = 0.10;
        let mut accent_border = accent;
        accent_border.a = 0.55;
        let mut row = h_flex().w_full().items_center().gap(px(8.0));
        for (provider, id) in [
            (CloudProvider::TencentCos, "tencent-cos"),
            (CloudProvider::AliyunOss, "aliyun-oss"),
        ] {
            let selected = self.provider == provider;
            let mut card = h_flex()
                .id(SharedString::from(format!("object-provider-{id}")))
                .debug_selector(move || format!("object-provider-{id}"))
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(7.0))
                .rounded_md()
                .border_1()
                .text_sm();
            if let Some(icon) = ramag_ui::icons::object_storage_brand_icon(id) {
                card = card.child(img(icon).size(px(16.0)).flex_none());
            }
            card = card.child(provider.display_name());
            card = if selected {
                card.bg(accent_tint)
                    .border_color(accent_border)
                    .text_color(accent)
            } else if self.saving {
                card.bg(secondary)
                    .border_color(border)
                    .text_color(muted)
                    .opacity(0.45)
            } else {
                card.bg(secondary)
                    .border_color(border)
                    .text_color(foreground)
                    .cursor_pointer()
                    .hover(move |card| card.border_color(accent_border))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_provider(provider, window, cx);
                    }))
            };
            row = row.child(card);
        }
        v_flex()
            .gap(px(8.0))
            .child(section_title("服务商", muted))
            .child(row)
    }
}

impl Render for AccountFormPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let secondary = cx.theme().secondary;
        let body_max_h = (window.viewport_size().height * 0.9 - px(210.0)).max(px(200.0));
        let editing = self.editing.is_some();
        let (access_key_id_label, access_key_secret_label) =
            credential_labels(self.provider, editing);
        let manual_rows = self
            .manual_buckets
            .iter()
            .enumerate()
            .map(|(index, bucket)| {
                h_flex()
                    .w_full()
                    .h(px(32.0))
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .rounded_md()
                    .bg(secondary)
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(format!(
                                "{} · {} · /{}",
                                bucket.name,
                                bucket.region,
                                bucket.root_prefix.as_deref().unwrap_or("")
                            )),
                    )
                    .child(
                        ramag_ui::clickable_button(SharedString::from(format!(
                            "remove-manual-bucket-{index}"
                        )))
                        .ghost()
                        .xsmall()
                        .label("移除")
                        .disabled(self.saving)
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                if !this.saving && index < this.manual_buckets.len() {
                                    this.manual_buckets.remove(index);
                                    this.feedback = None;
                                    cx.notify();
                                }
                            },
                        )),
                    )
            })
            .collect::<Vec<_>>();
        let feedback = self.feedback.clone();
        v_flex()
            .w_full()
            .pt(px(4.0))
            .child(
                div()
                    .id("object-account-form-body")
                    .w_full()
                    .max_h(body_max_h)
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .gap(px(18.0))
                            .child(self.render_provider_selector(cx))
                            .child(
                                v_flex()
                                    .gap(px(12.0))
                                    .child(section_title("账号", muted))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_end()
                                            .gap(px(16.0))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "object-account-name-field",
                                                "账号名称",
                                                Input::new(&self.name).disabled(self.saving),
                                            )))
                                            .child(
                                                h_flex()
                                                    .w(px(220.0))
                                                    .h(px(32.0))
                                                    .items_center()
                                                    .justify_between()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .child(ramag_ui::PRODUCTION_MODE_LABEL),
                                                    )
                                                    .child(
                                                        ramag_ui::clickable_switch(
                                                            "object-account-read-only",
                                                        )
                                                        .checked(self.production)
                                                        .disabled(self.saving)
                                                        .on_click(cx.listener(
                                                            |this, _: &bool, _, cx| {
                                                                if !this.saving {
                                                                    this.production =
                                                                        !this.production;
                                                                    this.feedback = None;
                                                                    cx.notify();
                                                                }
                                                            },
                                                        )),
                                                    ),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap(px(12.0))
                                    .child(section_title("认证", muted))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_end()
                                            .gap(px(12.0))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "object-access-key-id-field",
                                                access_key_id_label,
                                                Input::new(&self.access_key_id)
                                                    .disabled(self.saving),
                                            )))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "object-access-key-secret-field",
                                                access_key_secret_label,
                                                Input::new(&self.access_key_secret)
                                                    .disabled(self.saving),
                                            ))),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap(px(12.0))
                                    .child(section_title("Bucket 挂载（必填）", muted))
                                    .child(
                                        div().text_xs().text_color(muted).child(
                                            "请至少添加一个 Bucket；Endpoint 由服务商和 Region 自动生成。",
                                        ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_end()
                                            .gap(px(12.0))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "object-manual-bucket-field",
                                                "Bucket",
                                                Input::new(&self.bucket).disabled(self.saving),
                                            )))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "object-manual-region-field",
                                                "Region",
                                                Input::new(&self.region).disabled(self.saving),
                                            )))
                                            .child(div().flex_1().min_w_0().child(field(
                                                "object-manual-prefix-field",
                                                "Root Prefix",
                                                Input::new(&self.root_prefix)
                                                    .disabled(self.saving),
                                            )))
                                            .child(
                                                div()
                                                    .id("add-manual-bucket-layout")
                                                    .debug_selector(|| {
                                                        "add-manual-bucket-layout".into()
                                                    })
                                                    .h(px(32.0))
                                                    .flex()
                                                    .items_center()
                                                    .child(
                                                        ramag_ui::clickable_button(
                                                            "add-manual-bucket",
                                                        )
                                                        .outline()
                                                        .small()
                                                        .label("添加")
                                                        .disabled(self.saving)
                                                        .on_click(cx.listener(
                                                            |this, _: &ClickEvent, window, cx| {
                                                                this.add_manual_bucket(window, cx);
                                                            },
                                                        )),
                                                    ),
                                            ),
                                    )
                                    .children(manual_rows),
                            ),
                    ),
            )
            .child(div().h(px(1.0)).bg(border).my(px(10.0)))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().flex_1().min_w_0().when_some(
                        feedback,
                        |message, (text, error)| {
                            message.child(
                                div()
                                    .text_xs()
                                    .text_color(if error { cx.theme().danger } else { muted })
                                    .child(text),
                            )
                        },
                    ))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(8.0))
                            .child(
                                ramag_ui::clickable_button("cancel-object-account")
                                    .ghost()
                                    .small()
                                    .label("取消")
                                    .disabled(self.saving)
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.handle_cancel(window, cx);
                                        },
                                    )),
                            )
                            .child(
                                ramag_ui::clickable_button("save-object-account")
                                    .primary()
                                    .small()
                                    .label(if self.saving { "保存中…" } else { "保存" })
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.handle_save(cx);
                                    })),
                            ),
                    ),
            )
    }
}

fn section_title(label: &'static str, color: gpui::Hsla) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(color)
                .child(label),
        )
        .child(div().flex_1().h(px(1.0)).bg(color).opacity(0.12))
}

fn credential_labels(provider: CloudProvider, editing: bool) -> (&'static str, &'static str) {
    match (provider, editing) {
        (CloudProvider::TencentCos, false) => ("SecretId", "SecretKey"),
        (CloudProvider::TencentCos, true) => ("SecretId（留空不修改）", "SecretKey（留空不修改）"),
        (CloudProvider::AliyunOss, false) => ("AccessKey ID", "AccessKey Secret"),
        (CloudProvider::AliyunOss, true) => (
            "AccessKey ID（留空不修改）",
            "AccessKey Secret（留空不修改）",
        ),
    }
}

fn field(id: &'static str, label: &'static str, input: Input) -> impl IntoElement {
    v_flex()
        .id(id)
        .debug_selector(move || id.into())
        .w_full()
        .gap(px(6.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label),
        )
        .child(input)
}
