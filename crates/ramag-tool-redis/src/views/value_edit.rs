//! String key 的值全量替换弹窗（SET key value）

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{ConnectionConfig, MAX_REDIS_COMMAND_ARG_BYTES};
use tracing::{error, info};

use crate::views::bounded_input;

#[derive(Debug, Clone)]
pub enum ValueEditEvent {
    /// 保存成功
    Saved,
    Cancelled,
}

#[derive(Debug, Clone)]
enum SubmitState {
    Idle,
    Submitting,
    Failed(String),
}

pub struct ValueEditForm {
    service: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
    key: String,
    value_input: Entity<InputState>,
    state: SubmitState,
}

impl EventEmitter<ValueEditEvent> for ValueEditForm {}

impl ValueEditForm {
    pub fn is_submitting(&self) -> bool {
        matches!(self.state, SubmitState::Submitting)
    }

    pub fn new(
        service: Arc<RedisService>,
        config: ConnectionConfig,
        db: u8,
        key: String,
        initial_value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value_input = cx.new(|cx| {
            bounded_input(MAX_REDIS_COMMAND_ARG_BYTES, window, cx)
                .multi_line(true)
                .default_value(initial_value)
        });
        ramag_ui::enforce_multiline_input_byte_limit(
            &value_input,
            MAX_REDIS_COMMAND_ARG_BYTES,
            window,
            cx,
            |this, _, cx| {
                this.state = SubmitState::Failed(format!(
                    "字符串值最多保留 {} MiB，超出部分已截断",
                    MAX_REDIS_COMMAND_ARG_BYTES / 1024 / 1024
                ));
                cx.notify();
            },
        )
        .detach();
        Self {
            service,
            config,
            db,
            key,
            value_input,
            state: SubmitState::Idle,
        }
    }

    fn handle_save(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, SubmitState::Submitting) {
            return;
        }
        let value = self.value_input.read(cx).value().to_string();
        self.state = SubmitState::Submitting;
        cx.notify();

        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let key = self.key.clone();
        let argv = vec!["SET".to_string(), key.clone(), value];
        cx.spawn(async move |this, cx| {
            let result = svc.execute_command(&config, db, argv).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    info!(
                        operation = "redis_string_value_save",
                        connection_id = %config.id,
                        db,
                        key_bytes = key.len(),
                        "string value saved"
                    );
                    cx.emit(ValueEditEvent::Saved);
                }
                Err(e) => {
                    error!(
                        operation = "redis_string_value_save",
                        connection_id = %config.id,
                        db,
                        key_bytes = key.len(),
                        error = %e,
                        "save string value failed"
                    );
                    this.state = SubmitState::Failed(e.write_hint("保存失败"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn handle_cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(ValueEditEvent::Cancelled);
    }
}

impl Render for ValueEditForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;

        let err = match &self.state {
            SubmitState::Idle | SubmitState::Submitting => None,
            SubmitState::Failed(s) => Some(s.clone()),
        };
        let submitting = matches!(self.state, SubmitState::Submitting);

        v_flex()
            .w_full()
            .gap(px(14.0))
            .pt(px(4.0))
            .pb(px(4.0))
            .child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("Key: {}", self.key)),
            )
            .child(
                v_flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(muted_fg)
                            .child("新值"),
                    )
                    .child(
                        div().w_full().child(
                            Input::new(&self.value_input)
                                .h(px(220.0))
                                .disabled(submitting),
                        ),
                    ),
            )
            .child(div().h(px(1.0)).bg(border).my(px(2.0)))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(gpui::red())
                            .child(err.unwrap_or_default()),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .flex_none()
                            .child(
                                ramag_ui::clickable_button("ve-cancel")
                                    .ghost()
                                    .small()
                                    .label("取消")
                                    .disabled(submitting)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.handle_cancel(cx);
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("ve-save")
                                    .primary()
                                    .small()
                                    .label(if submitting { "保存中…" } else { "保存" })
                                    .disabled(submitting)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        if !matches!(this.state, SubmitState::Submitting) {
                                            this.handle_save(cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}
