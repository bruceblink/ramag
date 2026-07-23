//! Redis Key 新建表单。

use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, EventEmitter, Hsla, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, hsla, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, h_flex,
    input::{Input, InputState},
    v_flex,
};
use ramag_app::RedisService;
use ramag_domain::entities::{
    ConnectionConfig, MAX_REDIS_COMMAND_ARG_BYTES, MAX_REDIS_KEY_BYTES, RedisType,
    validate_redis_key,
};
use tracing::{error, info};

use crate::views::bounded_input;
use crate::views::form_shell::{SubmitState, deduplicate_preserving_order, form_footer};
use crate::views::lines_editor::{LinesEditor, LinesKind, PushDir};
use crate::views::pairs_editor::{PairsEditor, PairsKind};
use crate::views::ttl_picker::TtlPicker;

#[derive(Debug, Clone)]
pub enum KeyCreateEvent {
    /// TTL 后处理失败时携带警告，避免用户重复写入。
    Created {
        key: String,
        ttl_warning: Option<String>,
    },
    Cancelled,
}

const CREATE_TYPES: &[RedisType] = &[
    RedisType::String,
    RedisType::List,
    RedisType::Hash,
    RedisType::Set,
    RedisType::ZSet,
    RedisType::Stream,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostWriteTtl {
    Unchanged,
    Expire(i64),
    Persist,
}

enum CreateOutcome {
    Created,
    CreatedWithTtlWarning(String),
    Failed(String),
}

fn post_write_ttl(existing: RedisType, ttl: Option<i64>) -> PostWriteTtl {
    match ttl {
        Some(seconds) => PostWriteTtl::Expire(seconds),
        None if existing != RedisType::None => PostWriteTtl::Persist,
        None => PostWriteTtl::Unchanged,
    }
}

pub struct KeyCreateForm {
    service: Arc<RedisService>,
    config: ConnectionConfig,
    db: u8,
    selected_type: RedisType,
    key_name: Entity<InputState>,
    string_input: Entity<InputState>,
    list_editor: Entity<LinesEditor>,
    set_editor: Entity<LinesEditor>,
    hash_editor: Entity<PairsEditor>,
    zset_editor: Entity<PairsEditor>,
    stream_editor: Entity<PairsEditor>,
    ttl_picker: Entity<TtlPicker>,
    state: SubmitState,
}

impl EventEmitter<KeyCreateEvent> for KeyCreateForm {}

impl KeyCreateForm {
    pub fn is_submitting(&self) -> bool {
        self.state.is_submitting()
    }

    pub fn new(
        service: Arc<RedisService>,
        config: ConnectionConfig,
        db: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key_name = cx.new(|cx| {
            bounded_input(MAX_REDIS_KEY_BYTES, window, cx).placeholder("如 user:1001:cache")
        });
        // 多行输入高度必须设在 Input 上。
        let string_input = cx.new(|cx| {
            bounded_input(MAX_REDIS_COMMAND_ARG_BYTES, window, cx)
                .multi_line(true)
                .placeholder("字符串值（任意文本，可多行）")
        });
        ramag_ui::enforce_multiline_input_byte_limit(
            &string_input,
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
        let list_editor = cx.new(|cx| LinesEditor::new(LinesKind::List, window, cx));
        let set_editor = cx.new(|cx| LinesEditor::new(LinesKind::Set, window, cx));
        let hash_editor = cx.new(|cx| PairsEditor::new(PairsKind::Hash, window, cx));
        let zset_editor = cx.new(|cx| PairsEditor::new(PairsKind::ZSet, window, cx));
        let stream_editor = cx.new(|cx| PairsEditor::new(PairsKind::Stream, window, cx));
        let ttl_picker = cx.new(|cx| TtlPicker::new(window, cx));

        Self {
            service,
            config,
            db,
            selected_type: RedisType::String,
            key_name,
            string_input,
            list_editor,
            set_editor,
            hash_editor,
            zset_editor,
            stream_editor,
            ttl_picker,
            state: SubmitState::Idle,
        }
    }

    fn select_type(&mut self, t: RedisType, cx: &mut Context<Self>) {
        if !self.state.is_submitting() && self.selected_type != t {
            self.selected_type = t;
            if let SubmitState::Failed(_) = self.state {
                self.state = SubmitState::Idle;
            }
            cx.notify();
        }
    }

    fn build_argv_and_ttl(&self, cx: &gpui::App) -> Result<(Vec<String>, Option<i64>), String> {
        let key = self.key_name.read(cx).value().trim().to_string();
        if key.is_empty() {
            return Err("请填写 Key 名".into());
        }
        validate_redis_key(&key).map_err(|error| error.message().to_string())?;

        let argv: Vec<String> = match self.selected_type {
            RedisType::String => {
                let v = self.string_input.read(cx).value().to_string();
                vec!["SET".into(), key.clone(), v]
            }
            RedisType::List => {
                let editor = self.list_editor.read(cx);
                let elems = editor.collect(cx)?;
                if elems.is_empty() {
                    return Err("List 至少需要 1 个元素".into());
                }
                let cmd = match editor.push_dir() {
                    PushDir::Tail => "RPUSH",
                    PushDir::Head => "LPUSH",
                };
                let mut argv = vec![cmd.into(), key.clone()];
                argv.extend(elems);
                argv
            }
            RedisType::Set => {
                let elems = self.set_editor.read(cx).collect(cx)?;
                if elems.is_empty() {
                    return Err("Set 至少需要 1 个成员".into());
                }
                let dedup = deduplicate_preserving_order(elems);
                let mut argv = vec!["SADD".into(), key.clone()];
                argv.extend(dedup);
                argv
            }
            RedisType::Hash => {
                let pairs = self.hash_editor.read(cx).collect(cx)?;
                if pairs.is_empty() {
                    return Err("Hash 至少需要 1 个字段".into());
                }
                let mut argv = vec!["HSET".into(), key.clone()];
                for (f, v) in pairs {
                    argv.push(f);
                    argv.push(v);
                }
                argv
            }
            RedisType::ZSet => {
                let pairs = self.zset_editor.read(cx).collect(cx)?;
                if pairs.is_empty() {
                    return Err("ZSet 至少需要 1 个成员".into());
                }
                let mut argv = vec!["ZADD".into(), key.clone()];
                for (s, m) in pairs {
                    argv.push(s);
                    argv.push(m);
                }
                argv
            }
            RedisType::Stream => {
                let pairs = self.stream_editor.read(cx).collect(cx)?;
                if pairs.is_empty() {
                    return Err("Stream 至少需要 1 个字段".into());
                }
                let mut argv = vec!["XADD".into(), key.clone(), "*".into()];
                for (f, v) in pairs {
                    argv.push(f);
                    argv.push(v);
                }
                argv
            }
            RedisType::None => return Err("未知类型".into()),
        };

        let ttl = self.ttl_picker.read(cx).collect(cx)?;
        Ok((argv, ttl))
    }

    fn handle_create(&mut self, cx: &mut Context<Self>) {
        if self.state.is_submitting() {
            return;
        }
        let (argv, ttl) = match self.build_argv_and_ttl(cx) {
            Ok(t) => t,
            Err(e) => {
                self.state = SubmitState::Failed(e);
                cx.notify();
                return;
            }
        };
        let key = self.key_name.read(cx).value().trim().to_string();
        let intended_type = self.selected_type;

        self.set_child_editors_disabled(true, cx);
        self.state = SubmitState::Submitting;
        cx.notify();

        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            // 允许新 Key 与同类型写入，拒绝跨类型覆盖。
            let existing = match svc.key_type(&config, db, &key).await {
                Ok(existing) => existing,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.set_child_editors_disabled(false, cx);
                        error!(error = %error, key_bytes = key.len(), "create key precheck failed");
                        this.state =
                            SubmitState::Failed(error.write_hint("创建前检查 Key 类型失败"));
                        cx.notify();
                    });
                    return;
                }
            };
            if existing != RedisType::None && existing != intended_type {
                let msg = format!(
                    "已存在「{}」类型的 key「{key}」，不能用「{}」类型覆盖。请先删除原 key 或换名。",
                    existing.label(),
                    intended_type.label(),
                );
                let _ = this.update(cx, |this, cx| {
                    this.set_child_editors_disabled(false, cx);
                    error!(
                        existing_type = existing.label(),
                        intended_type = intended_type.label(),
                        key_bytes = key.len(),
                        "create key precheck found type conflict"
                    );
                    this.state = SubmitState::Failed(msg);
                    cx.notify();
                });
                return;
            }

            let write_result = svc.execute_command(&config, db, argv).await;
            let ttl_action = post_write_ttl(existing, ttl);
            let outcome = match write_result {
                Ok(_) => match ttl_action {
                    PostWriteTtl::Expire(seconds) => match svc
                        .set_ttl(&config, db, &key, Some(seconds))
                        .await
                    {
                        Ok(true) => CreateOutcome::Created,
                        Ok(false) => CreateOutcome::CreatedWithTtlWarning(
                            "Key 已创建，但 TTL 未生效；Key 可能已被并发删除".into(),
                        ),
                        Err(e) => CreateOutcome::CreatedWithTtlWarning(format!(
                            "Key 已创建，但 TTL 设置失败：{e}"
                        )),
                    },
                    // 同类型旧 Key 允许合并；用户选“永久”时要清掉它原有的 TTL。
                    PostWriteTtl::Persist => match svc.set_ttl(&config, db, &key, None).await {
                        // false 也可能表示本来就没有 TTL，此时目标状态已经满足。
                        Ok(_) => CreateOutcome::Created,
                        Err(e) => CreateOutcome::CreatedWithTtlWarning(format!(
                            "Key 已创建，但清除原 TTL 失败：{e}"
                        )),
                    },
                    PostWriteTtl::Unchanged => CreateOutcome::Created,
                },
                Err(e) => CreateOutcome::Failed(e.to_string()),
            };
            let _ = this.update(cx, |this, cx| match outcome {
                CreateOutcome::Created => {
                    info!(key_bytes = key.len(), ?ttl, "key created");
                    cx.emit(KeyCreateEvent::Created {
                        key: key.clone(),
                        ttl_warning: None,
                    });
                }
                CreateOutcome::CreatedWithTtlWarning(warning) => {
                    tracing::warn!(key_bytes = key.len(), "key created with TTL warning");
                    cx.emit(KeyCreateEvent::Created {
                        key: key.clone(),
                        ttl_warning: Some(warning),
                    });
                }
                CreateOutcome::Failed(msg) => {
                    this.set_child_editors_disabled(false, cx);
                    error!(error = %msg, "create key failed");
                    this.state = SubmitState::Failed(msg);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn handle_cancel(&mut self, cx: &mut Context<Self>) {
        if self.state.is_submitting() {
            return;
        }
        cx.emit(KeyCreateEvent::Cancelled);
    }

    fn set_child_editors_disabled(&self, disabled: bool, cx: &mut Context<Self>) {
        self.list_editor
            .update(cx, |editor, cx| editor.set_disabled(disabled, cx));
        self.set_editor
            .update(cx, |editor, cx| editor.set_disabled(disabled, cx));
        self.hash_editor
            .update(cx, |editor, cx| editor.set_disabled(disabled, cx));
        self.zset_editor
            .update(cx, |editor, cx| editor.set_disabled(disabled, cx));
        self.stream_editor
            .update(cx, |editor, cx| editor.set_disabled(disabled, cx));
        self.ttl_picker
            .update(cx, |picker, cx| picker.set_disabled(disabled, cx));
    }

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
                    .child(section_title("Key 名", muted_fg, None))
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

/// Redis 类型标志色（与 `key_tree::type_color_solid` 同款，刻意不跨模块复用以避免破坏分层）
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

#[cfg(test)]
mod tests {
    use super::{PostWriteTtl, post_write_ttl};
    use ramag_domain::entities::RedisType;

    #[test]
    fn ttl_plan_preserves_new_permanent_key_without_extra_command() {
        assert_eq!(
            post_write_ttl(RedisType::None, None),
            PostWriteTtl::Unchanged
        );
    }

    #[test]
    fn ttl_plan_persists_existing_key_or_sets_expiration() {
        assert_eq!(post_write_ttl(RedisType::Hash, None), PostWriteTtl::Persist);
        assert_eq!(
            post_write_ttl(RedisType::None, Some(300)),
            PostWriteTtl::Expire(300)
        );
    }
}
