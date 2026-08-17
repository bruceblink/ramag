use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Window};
use gpui_component::input::InputState;
use ramag_app::RedisService;
use ramag_domain::entities::{
    ConnectionConfig, MAX_REDIS_COMMAND_ARG_BYTES, MAX_REDIS_KEY_BYTES, RedisType,
    validate_redis_key,
};
use tracing::{error, info, warn};

use crate::views::bounded_input;
use crate::views::form_shell::{SubmitState, deduplicate_preserving_order};
use crate::views::lines_editor::{LinesEditor, LinesKind, PushDir};
use crate::views::pairs_editor::{PairsEditor, PairsKind};
use crate::views::ttl_picker::TtlPicker;

#[derive(Debug, Clone)]
pub enum KeyCreateEvent {
    /// TTL 更新失败时附带警告。
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

mod render;

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
        let string_input = cx.new(|cx| {
            bounded_input(MAX_REDIS_COMMAND_ARG_BYTES, window, cx)
                .multi_line(true)
                .placeholder("字符串值（可多行）")
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
        // 键支持前后空格，不能 trim。
        let key = self.key_name.read(cx).value().to_string();
        if key.is_empty() {
            return Err("请填写键名".into());
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
        let key = self.key_name.read(cx).value().to_string();
        let intended_type = self.selected_type;

        self.set_child_editors_disabled(true, cx);
        self.state = SubmitState::Submitting;
        cx.notify();

        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            // 禁止跨类型覆盖。
            let existing = match svc.key_type(&config, db, &key).await {
                Ok(existing) => existing,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.set_child_editors_disabled(false, cx);
                        error!(
                            operation = "redis_key_create",
                            connection_id = %config.id,
                            db,
                            key_type = intended_type.label(),
                            key_bytes = key.len(),
                            error = %error,
                            "create key precheck failed"
                        );
                        this.state = SubmitState::Failed(error.write_hint("检查键类型失败"));
                        cx.notify();
                    });
                    return;
                }
            };
            if existing != RedisType::None && existing != intended_type {
                let msg = format!(
                    "键「{key}」已是「{}」类型，不能创建为「{}」。请删除原键或换名。",
                    existing.label(),
                    intended_type.label(),
                );
                let _ = this.update(cx, |this, cx| {
                    this.set_child_editors_disabled(false, cx);
                    warn!(
                        operation = "redis_key_create",
                        connection_id = %config.id,
                        db,
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
                    PostWriteTtl::Expire(seconds) => {
                        match svc.set_ttl(&config, db, &key, Some(seconds)).await {
                            Ok(true) => CreateOutcome::Created,
                            Ok(false) => CreateOutcome::CreatedWithTtlWarning(
                                "键已创建，但 TTL 未生效；可能已被并发删除".into(),
                            ),
                            Err(e) => CreateOutcome::CreatedWithTtlWarning(format!(
                                "键已创建，但 TTL 设置失败：{e}"
                            )),
                        }
                    }
                    PostWriteTtl::Persist => match svc.set_ttl(&config, db, &key, None).await {
                        Ok(_) => CreateOutcome::Created,
                        Err(e) => CreateOutcome::CreatedWithTtlWarning(format!(
                            "键已创建，但清除原 TTL 失败：{e}"
                        )),
                    },
                    PostWriteTtl::Unchanged => CreateOutcome::Created,
                },
                Err(e) => CreateOutcome::Failed(e.to_string()),
            };
            let _ = this.update(cx, |this, cx| match outcome {
                CreateOutcome::Created => {
                    info!(
                        operation = "redis_key_create",
                        connection_id = %config.id,
                        db,
                        key_type = intended_type.label(),
                        key_bytes = key.len(),
                        ttl_seconds = ?ttl,
                        "key created"
                    );
                    cx.emit(KeyCreateEvent::Created {
                        key: key.clone(),
                        ttl_warning: None,
                    });
                }
                CreateOutcome::CreatedWithTtlWarning(warning) => {
                    warn!(
                        operation = "redis_key_create",
                        connection_id = %config.id,
                        db,
                        key_type = intended_type.label(),
                        key_bytes = key.len(),
                        ttl_seconds = ?ttl,
                        warning = %warning,
                        "key created with TTL warning"
                    );
                    cx.emit(KeyCreateEvent::Created {
                        key: key.clone(),
                        ttl_warning: Some(warning),
                    });
                }
                CreateOutcome::Failed(msg) => {
                    this.set_child_editors_disabled(false, cx);
                    error!(
                        operation = "redis_key_create",
                        connection_id = %config.id,
                        db,
                        key_type = intended_type.label(),
                        key_bytes = key.len(),
                        ttl_seconds = ?ttl,
                        error = %msg,
                        "create key failed"
                    );
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
