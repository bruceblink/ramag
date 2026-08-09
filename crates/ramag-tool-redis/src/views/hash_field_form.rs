//! Hash 单字段：新增 field+value 或编辑（锁 field）。两种都走 `HSET key field value`

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme,
    input::{Input, InputState},
    v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{ConnectionConfig, MAX_REDIS_COMMAND_ARG_BYTES};
use tracing::{error, info};

use crate::views::bounded_input;
use crate::views::form_shell::{SubmitState, form_footer};

#[derive(Debug, Clone)]
pub enum HashFieldFormMode {
    Add,
    Edit { field: String },
}

#[derive(Debug, Clone)]
pub enum HashFieldFormEvent {
    /// 提交成功，返回 field，让上层重载详情
    Saved {
        field: String,
    },
    Cancelled,
}

pub struct HashFieldForm {
    service: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
    key: String,
    mode: HashFieldFormMode,
    field_input: Entity<InputState>,
    value_input: Entity<InputState>,
    state: SubmitState,
}

impl EventEmitter<HashFieldFormEvent> for HashFieldForm {}

impl HashFieldForm {
    pub fn is_submitting(&self) -> bool {
        self.state.is_submitting()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        service: Arc<RedisService>,
        config: ConnectionConfig,
        db: u8,
        key: String,
        mode: HashFieldFormMode,
        initial_value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_field = match &mode {
            HashFieldFormMode::Add => String::new(),
            HashFieldFormMode::Edit { field } => field.clone(),
        };
        let field_input = cx.new(|cx| {
            bounded_input(MAX_REDIS_COMMAND_ARG_BYTES, window, cx)
                .placeholder("字段名（如 name）")
                .default_value(initial_field)
        });
        let value_input = cx.new(|cx| {
            bounded_input(MAX_REDIS_COMMAND_ARG_BYTES, window, cx)
                .multi_line(true)
                .placeholder("字段值（任意文本，可多行）")
                .default_value(initial_value)
        });
        ramag_ui::enforce_multiline_input_byte_limit(
            &value_input,
            MAX_REDIS_COMMAND_ARG_BYTES,
            window,
            cx,
            |this, _, cx| {
                this.state = SubmitState::Failed(format!(
                    "字段值最多保留 {} MiB，超出部分已截断",
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
            mode,
            field_input,
            value_input,
            state: SubmitState::Idle,
        }
    }

    fn handle_save(&mut self, cx: &mut Context<Self>) {
        if self.state.is_submitting() {
            return;
        }
        let field = match &self.mode {
            HashFieldFormMode::Edit { field } => field.clone(),
            // Redis 字段名是二进制安全参数，不能静默删除合法的前后空格。
            HashFieldFormMode::Add => self.field_input.read(cx).value().to_string(),
        };
        if field.is_empty() {
            self.state = SubmitState::Failed("请填写字段名".into());
            cx.notify();
            return;
        }
        let value = self.value_input.read(cx).value().to_string();

        self.state = SubmitState::Submitting;
        cx.notify();
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let key = self.key.clone();
        let key_bytes = key.len();
        let argv = vec!["HSET".to_string(), key, field.clone(), value];
        cx.spawn(async move |this, cx| {
            let result = svc.execute_command(&config, db, argv).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    info!(
                        operation = "redis_hash_field_save",
                        connection_id = %config.id,
                        db,
                        key_bytes,
                        field_bytes = field.len(),
                        "hash field saved"
                    );
                    cx.emit(HashFieldFormEvent::Saved { field });
                }
                Err(e) => {
                    error!(
                        operation = "redis_hash_field_save",
                        connection_id = %config.id,
                        db,
                        key_bytes,
                        field_bytes = field.len(),
                        error = %e,
                        "save hash field failed"
                    );
                    this.state = SubmitState::Failed(e.write_hint("保存失败"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn handle_cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(HashFieldFormEvent::Cancelled);
    }
}

impl Render for HashFieldForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;

        let is_edit = matches!(self.mode, HashFieldFormMode::Edit { .. });
        let submitting = self.state.is_submitting();

        let field_block = if is_edit {
            v_flex()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(muted_fg)
                        .child("字段（不可修改）"),
                )
                .child(
                    div()
                        .w_full()
                        .opacity(0.6)
                        .child(Input::new(&self.field_input).disabled(true)),
                )
        } else {
            v_flex()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(muted_fg)
                        .child("字段名"),
                )
                .child(
                    div()
                        .w_full()
                        .child(Input::new(&self.field_input).disabled(submitting)),
                )
        };

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
            .child(field_block)
            .child(
                v_flex()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(muted_fg)
                            .child("值"),
                    )
                    .child(
                        div().w_full().child(
                            Input::new(&self.value_input)
                                .h(px(180.0))
                                .disabled(submitting),
                        ),
                    ),
            )
            .child(div().h(px(1.0)).bg(border).my(px(2.0)))
            .child(form_footer(
                "hf",
                "保存",
                &self.state,
                |this, _: &ClickEvent, _, cx| this.handle_cancel(cx),
                |this, _: &ClickEvent, _, cx| {
                    if !this.state.is_submitting() {
                        this.handle_save(cx);
                    }
                },
                cx,
            ))
    }
}
