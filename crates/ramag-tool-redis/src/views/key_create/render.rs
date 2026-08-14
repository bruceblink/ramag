//! Redis 键创建表单渲染。

use gpui::{
    AnyElement, ClickEvent, Context, Hsla, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, hsla, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, input::Input, v_flex};
use ramag_domain::entities::RedisType;

use super::{CREATE_TYPES, KeyCreateForm};
use crate::views::form_shell::form_footer;

impl KeyCreateForm {
    fn render_editor(&self, disabled: bool) -> AnyElement {
        match self.selected_type {
            RedisType::String => Input::new(&self.string_input)
                .h(px(220.0))
                .disabled(disabled)
                .into_any_element(),
            RedisType::List => self.list_editor.clone().into_any_element(),
            RedisType::Set => self.set_editor.clone().into_any_element(),
            RedisType::Hash => self.hash_editor.clone().into_any_element(),
            RedisType::ZSet => self.zset_editor.clone().into_any_element(),
            RedisType::Stream => self.stream_editor.clone().into_any_element(),
            RedisType::None => div().into_any_element(),
        }
    }
}

impl Render for KeyCreateForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let secondary_bg = theme.secondary;
        let submitting = self.state.is_submitting();

        let current_color = redis_type_color(self.selected_type);
        let mut card_bg = secondary_bg;
        card_bg.a = 0.45;

        let mut type_row = h_flex().w_full().items_center().gap(px(6.0));
        for t in CREATE_TYPES {
            let is_selected = self.selected_type == *t;
            let kind = *t;
            let label = t.label();
            let color = redis_type_color(kind);
            let mut tint = color;
            tint.a = 0.12;
            let mut soft_border = color;
            soft_border.a = 0.55;

            let dot = div()
                .w(px(7.0))
                .h(px(7.0))
                .rounded_full()
                .bg(color)
                .flex_none();

            let btn_id = SharedString::from(format!("ktype-{}", t.as_scan_arg()));
            let mut btn = h_flex()
                .id(btn_id)
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded_md()
                .border_1()
                .text_sm()
                .child(dot)
                .child(label);
            if is_selected {
                btn = btn
                    .bg(tint)
                    .border_color(soft_border)
                    .text_color(color)
                    .font_weight(gpui::FontWeight::SEMIBOLD);
            } else if !submitting {
                btn = btn
                    .bg(secondary_bg)
                    .border_color(border)
                    .text_color(fg)
                    .cursor_pointer()
                    .hover(move |this| this.border_color(soft_border))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_type(kind, cx);
                    }));
            } else {
                btn = btn.bg(secondary_bg).border_color(border).text_color(fg);
            }
            if submitting {
                btn = btn.opacity(0.55);
            }
            type_row = type_row.child(btn);
        }

        let value_section_title = format!("{} 值", self.selected_type.label());

        v_flex()
            .w_full()
            .gap(px(18.0))
            .pt(px(2.0))
            .pb(px(2.0))
            .child(
                v_flex()
                    .gap(px(8.0))
                    .child(section_title("键名", muted_fg, None))
                    .child(
                        div()
                            .w_full()
                            .child(Input::new(&self.key_name).disabled(submitting)),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(8.0))
                    .child(section_title("类型", muted_fg, None))
                    .child(type_row),
            )
            .child(
                v_flex()
                    .gap(px(10.0))
                    .child(section_title(
                        &value_section_title,
                        muted_fg,
                        Some(current_color),
                    ))
                    .child(
                        div()
                            .w_full()
                            .p(px(14.0))
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .bg(card_bg)
                            .child(self.render_editor(submitting)),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(8.0))
                    .child(section_title("TTL", muted_fg, None))
                    .child(self.ttl_picker.clone()),
            )
            .child(div().h(px(1.0)).bg(border).my(px(2.0)))
            .child(form_footer(
                "kc",
                "创建",
                &self.state,
                |this, _: &ClickEvent, _, cx| this.handle_cancel(cx),
                |this, _: &ClickEvent, _, cx| {
                    if !this.state.is_submitting() {
                        this.handle_create(cx);
                    }
                },
                cx,
            ))
    }
}

fn section_title(text: &str, muted_fg: Hsla, dot_color: Option<Hsla>) -> impl IntoElement {
    let mut row = h_flex().items_center().gap(px(8.0));
    if let Some(c) = dot_color {
        row = row.child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(c).flex_none());
    }
    row.child(
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(muted_fg)
            .child(text.to_string()),
    )
    .child(div().flex_1().h(px(1.0)).bg(muted_fg).opacity(0.12))
}

fn redis_type_color(t: RedisType) -> Hsla {
    match t {
        RedisType::String => hsla(210.0 / 360.0, 0.6, 0.55, 1.0),
        RedisType::List => hsla(140.0 / 360.0, 0.5, 0.5, 1.0),
        RedisType::Hash => hsla(280.0 / 360.0, 0.55, 0.6, 1.0),
        RedisType::Set => hsla(40.0 / 360.0, 0.85, 0.55, 1.0),
        RedisType::ZSet => hsla(20.0 / 360.0, 0.7, 0.55, 1.0),
        RedisType::Stream => hsla(330.0 / 360.0, 0.55, 0.55, 1.0),
        RedisType::None => hsla(0.0, 0.0, 0.5, 1.0),
    }
}
